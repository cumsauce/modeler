//! Bakes a graph's `output` node into PBR channel images.

use rayon::prelude::*;
use serde_json::Value;

use super::eval::{self, Evaluated, Samples};
use super::node::{self, Field, In};
use crate::params::Params;
use crate::raster::Canvas;

/// The PBR channels of a bake, one value per sample (row-major, top first).
#[derive(Debug, Clone)]
pub struct Channels {
    pub w: usize,
    pub h: usize,
    /// Linear-light base colour, 0..1.
    pub base_color: Vec<[f32; 3]>,
    pub roughness: Vec<f32>,
    pub metallic: Vec<f32>,
    /// Tangent-space unit normal, z up.
    pub normal: Vec<[f32; 3]>,
    /// Height in meters (0 when the graph gives none).
    pub height: Vec<f32>,
    pub ao: Vec<f32>,
    pub emissive: Vec<[f32; 3]>,
    pub opacity: Vec<f32>,
    /// Which channels the graph actually drives (the rest are defaults).
    pub driven: Vec<&'static str>,
    /// Nodes computed (not served from the cache).
    pub runs: usize,
}

/// Every channel name, in display order.
pub const CHANNELS: &[&str] = &[
    "base_color",
    "roughness",
    "metallic",
    "normal",
    "height",
    "ao",
    "emissive",
    "opacity",
];

/// Evaluates and bakes `nodes` over `s`.
pub fn bake(
    nodes: &[Value],
    params: &Params,
    s: &Samples,
    cached: bool,
) -> Result<Channels, String> {
    let ev = eval::evaluate(nodes, params, s, cached)?;
    channels(&ev, s)
}

/// Assembles the channels from an evaluated graph.
pub fn channels(ev: &Evaluated, s: &Samples) -> Result<Channels, String> {
    let n = s.len();
    let o = ev.output().cloned().unwrap_or_default();
    let mut driven = Vec::new();
    let mut ch = |name: &'static str, i: &Option<In>, default: Field| -> Result<Field, String> {
        match i {
            Some(i) => {
                driven.push(name);
                ev.field(i).map_err(|e| format!("output.{name}: {e}"))
            }
            None => Ok(default),
        }
    };
    let base = ch("base_color", &o.base_color, Field::V(vec![0.8; 3]))?;
    let rough = ch("roughness", &o.roughness, Field::F(0.5))?;
    let metal = ch("metallic", &o.metallic, Field::F(0.0))?;
    let height = ch("height", &o.height, Field::F(0.0))?;
    let nrm = ch("normal", &o.normal, Field::V(vec![0.0, 0.0, 1.0]))?;
    let ao = ch("ao", &o.ao, Field::F(1.0))?;
    let emis = ch("emissive", &o.emissive, Field::V(vec![0.0; 3]))?;
    let opac = ch("opacity", &o.opacity, Field::F(1.0))?;
    let from_height = o.normal.is_none() && o.height.is_some();
    if from_height {
        driven.push("normal");
    }
    let k = o.normal_strength.unwrap_or(1.0);
    let par3 = |f: &Field| -> Vec<[f32; 3]> { (0..n).into_par_iter().map(|i| f.v3(i)).collect() };
    let par1 = |f: &Field| -> Vec<f32> { (0..n).into_par_iter().map(|i| f.f(i)).collect() };
    let normal: Vec<[f32; 3]> = (0..n)
        .into_par_iter()
        .map(|i| {
            if from_height {
                node::height_normal(s, &height, i, k)
            } else {
                let v = nrm.v3(i);
                let v = [v[0] * k, v[1] * k, v[2]];
                let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
                if l > 1e-9 {
                    v.map(|c| c / l)
                } else {
                    [0.0, 0.0, 1.0]
                }
            }
        })
        .collect();
    Ok(Channels {
        w: s.w,
        h: s.h,
        base_color: par3(&base),
        roughness: par1(&rough),
        metallic: par1(&metal),
        normal,
        height: par1(&height),
        ao: par1(&ao),
        emissive: par3(&emis),
        opacity: par1(&opac),
        driven,
        runs: ev.runs,
    })
}

#[inline]
fn u8c(x: f32) -> u8 {
    (x.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// Linear to sRGB transfer (for colour textures and previews).
#[inline]
pub fn to_srgb(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    if x <= 0.003_130_8 {
        x * 12.92
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    }
}

impl Channels {
    /// One channel as an RGBA image: colours in sRGB, scalars as grey,
    /// normals as the usual (n + 1) / 2 encoding, height normalised to its
    /// own range.
    pub fn image(&self, name: &str) -> Result<Canvas, String> {
        let n = self.w * self.h;
        let mut px = vec![255u8; n * 4];
        let (hlo, hhi) = self
            .height
            .iter()
            .fold((f32::MAX, f32::MIN), |(a, b), &h| (a.min(h), b.max(h)));
        let span = (hhi - hlo).max(1e-9);
        let rgb: Box<dyn Fn(usize) -> [u8; 3] + Sync> = match name {
            "base_color" => Box::new(|i| self.base_color[i].map(|c| u8c(to_srgb(c)))),
            "emissive" => Box::new(|i| self.emissive[i].map(|c| u8c(to_srgb(c)))),
            "normal" => Box::new(|i| self.normal[i].map(|c| u8c(c * 0.5 + 0.5))),
            "roughness" => Box::new(|i| [u8c(self.roughness[i]); 3]),
            "metallic" => Box::new(|i| [u8c(self.metallic[i]); 3]),
            "ao" => Box::new(|i| [u8c(self.ao[i]); 3]),
            "opacity" => Box::new(|i| [u8c(self.opacity[i]); 3]),
            "height" => Box::new(move |i| [u8c((self.height[i] - hlo) / span); 3]),
            o => {
                return Err(format!(
                    "unknown channel `{o}` (one of: {})",
                    CHANNELS.join(", ")
                ));
            }
        };
        px.par_chunks_mut(4).enumerate().for_each(|(i, p)| {
            let c = rgb(i);
            p[..3].copy_from_slice(&c);
        });
        Ok(Canvas::from_rgba(self.w, self.h, px))
    }

    /// Labelled channel thumbnails side by side (`cols` per row), each
    /// `cell` px square, for a headless look at a material.
    pub fn grid(&self, names: &[&str], cell: usize, cols: usize) -> Result<Canvas, String> {
        let cols = cols.clamp(1, names.len().max(1));
        let rows = names.len().div_ceil(cols).max(1);
        let gap = 4;
        let (w, h) = (cols * (cell + gap) + gap, rows * (cell + gap) + gap);
        let mut out = Canvas::new(w, h, [24, 24, 28, 255]);
        for (k, name) in names.iter().enumerate() {
            let img = self.image(name)?;
            let (x0, y0) = (
                gap + (k % cols) * (cell + gap),
                gap + (k / cols) * (cell + gap),
            );
            for y in 0..cell {
                let sy = y * img.h / cell;
                for x in 0..cell {
                    let sx = x * img.w / cell;
                    let si = (sy * img.w + sx) * 4;
                    let di = ((y0 + y) * w + x0 + x) * 4;
                    out.px[di..di + 4].copy_from_slice(&img.px[si..si + 4]);
                }
            }
            out.text(x0 + 2, y0 + 2, name, 1);
        }
        Ok(out)
    }

    /// How seamy the tile's wrap edges are: per edge texel, the jump across
    /// the wrap over the mean jump just inside on both sides, and the median
    /// of that over every edge texel. ~1 when the tile repeats cleanly (the
    /// seam looks like any other texel step, even for fine noise); a
    /// pattern that does not wrap lands well above 2.
    pub fn seam_ratio(&self, name: &str) -> Result<f32, String> {
        let img = self.image(name)?;
        let (w, h) = (img.w, img.h);
        if w < 3 || h < 3 {
            return Ok(0.0);
        }
        let g = |x: usize, y: usize| -> [f32; 3] {
            let i = (y * w + x) * 4;
            [img.px[i] as f32, img.px[i + 1] as f32, img.px[i + 2] as f32]
        };
        let d = |a: [f32; 3], b: [f32; 3]| (0..3).map(|k| (a[k] - b[k]).abs()).fold(0.0, f32::max);
        let mut r = Vec::with_capacity(w + h);
        for y in 0..h {
            let seam = d(g(w - 1, y), g(0, y));
            let local = 0.5 * (d(g(w - 2, y), g(w - 1, y)) + d(g(0, y), g(1, y)));
            r.push(seam / (local + 2.0));
        }
        for x in 0..w {
            let seam = d(g(x, h - 1), g(x, 0));
            let local = 0.5 * (d(g(x, h - 2), g(x, h - 1)) + d(g(x, 0), g(x, 1)));
            r.push(seam / (local + 2.0));
        }
        r.sort_by(f32::total_cmp);
        Ok(r[r.len() / 2])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bricks() -> Vec<Value> {
        json!([
            {"type": "tile_grid", "name": "g", "size": [0.25, 0.0625], "stagger": 0.5},
            {"type": "rect", "name": "brick", "coords": "g.local", "size": [0.24, 0.0525], "round": 0.004},
            {"type": "mask", "name": "m", "sdf": "brick"},
            {"type": "noise", "name": "n", "size": 0.05, "octaves": 4, "seed": 3},
            {"type": "random", "name": "tint", "id": "g.id", "out": "color", "min": 0.35, "max": 0.6},
            {"type": "math", "name": "brick_col", "op": "mul", "a": "tint", "b": [1.0, 0.5, 0.4]},
            {"type": "blend", "name": "col", "a": "#a8a196", "b": "brick_col", "opacity": "m"},
            {"type": "math", "name": "h", "op": "mul", "a": "m", "b": 0.006},
            {"type": "remap", "name": "rough", "input": "n", "from": [0, 1], "to": [0.7, 0.95]},
            {"type": "output", "name": "out", "base_color": "col", "roughness": "rough", "height": "h"}
        ])
        .as_array()
        .cloned()
        .unwrap_or_default()
    }

    #[test]
    fn bricks_bake_tileable_in_range_and_deterministic_across_threads() {
        let s = Samples::tile(512, 1.0);
        let p = Params::new();
        let a = bake(&bricks(), &p, &s, false).unwrap_or_else(|e| panic!("{e}"));
        for ch in ["base_color", "roughness", "normal"] {
            let r = a.seam_ratio(ch).unwrap_or_else(|e| panic!("{e}"));
            assert!(r <= 1.5, "{ch} seam ratio {r}");
        }
        assert!(a.roughness.iter().all(|r| (0.7..=0.95).contains(r)));
        assert!(a.driven.contains(&"normal"));
        // Bricks cover most of the area, mortar the rest.
        let m = node::srgb_to_linear(168.0 / 255.0);
        let mortar = a
            .base_color
            .iter()
            .filter(|c| (c[0] - m).abs() < 0.02)
            .count();
        let frac = mortar as f32 / a.base_color.len() as f32;
        assert!((0.05..0.35).contains(&frac), "mortar fraction {frac}");
        // One thread gives bit-identical results.
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap_or_else(|e| panic!("{e}"));
        let b = pool
            .install(|| bake(&bricks(), &p, &s, false))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(a.base_color, b.base_color);
        assert_eq!(a.normal, b.normal);
        assert_eq!(a.roughness, b.roughness);
    }

    #[test]
    fn a_tweak_reruns_only_the_node_and_its_readers() {
        let s = Samples::tile(32, 0.73);
        let p = Params::new();
        let mut g = bricks();
        let first = bake(&g, &p, &s, true).unwrap_or_else(|e| panic!("{e}"));
        assert!(first.runs <= g.len());
        let again = bake(&g, &p, &s, true).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(again.runs, 0, "unchanged graph is fully cached");
        // Change the roughness remap: it and the output re-run, nothing else.
        g[8] = json!({"type": "remap", "name": "rough", "input": "n", "from": [0, 1], "to": [0.6, 0.9]});
        let tweak = bake(&g, &p, &s, true).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(tweak.runs, 2, "remap + output");
        let fresh = bake(&g, &p, &s, false).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(tweak.roughness, fresh.roughness);
    }

    #[test]
    fn errors_name_the_node_and_field() {
        let s = Samples::tile(8, 1.0);
        let p = Params::new();
        let e = |g: Value| {
            bake(
                g.as_array().map(|a| a.as_slice()).unwrap_or(&[]),
                &p,
                &s,
                false,
            )
            .err()
            .unwrap_or_default()
        };
        assert!(
            e(json!([{"type": "noise", "name": "n", "size": 0.1, "octave": 3}])).contains("octave")
        );
        assert!(e(json!([{"type": "circle", "name": "c", "radius": "rr"}])).contains("reads `rr`"));
        assert!(e(json!([{"type": "worley", "name": "w", "size": 0.1}, {"type": "math", "name": "m", "op": "add", "a": "w.f3", "b": 1}])).contains("ports: f1, f2, edge, id"));
        assert!(e(json!([{"type": "math", "name": "a", "op": "add", "a": "b", "b": 1}, {"type": "math", "name": "b", "op": "add", "a": "a", "b": 1}])).contains("cycle"));
        assert!(e(json!([{"type": "blob", "name": "x"}])).contains("unknown node type"));
    }

    #[test]
    fn params_drive_node_fields() {
        let s = Samples::tile(16, 1.0);
        let mut p = Params::new();
        p.insert("r".into(), 0.8);
        let g = json!([{"type": "output", "name": "o", "roughness": "$r"}]);
        let c = bake(
            g.as_array().map(|a| a.as_slice()).unwrap_or(&[]),
            &p,
            &s,
            false,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(c.roughness.iter().all(|r| (r - 0.8).abs() < 1e-6));
    }
}
