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
    /// Whole-surface factors (clearcoat, anisotropy, transmission, ...).
    pub extras: Extras,
    /// Nodes computed (not served from the cache).
    pub runs: usize,
}

/// Scalar material factors beyond the textured channels; `None` = the
/// renderer's default.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Extras {
    pub clearcoat: Option<f32>,
    pub clearcoat_roughness: Option<f32>,
    pub anisotropy: Option<f32>,
    /// Degrees.
    pub anisotropy_rotation: Option<f32>,
    pub transmission: Option<f32>,
    pub specular_transmission: Option<f32>,
    pub thickness: Option<f32>,
    pub ior: Option<f32>,
    pub alpha: Option<String>,
    pub alpha_cutoff: Option<f32>,
    pub double_sided: Option<bool>,
}

impl Extras {
    pub fn of(o: &node::Output) -> Extras {
        Extras {
            clearcoat: o.clearcoat,
            clearcoat_roughness: o.clearcoat_roughness,
            anisotropy: o.anisotropy,
            anisotropy_rotation: o.anisotropy_rotation,
            transmission: o.transmission,
            specular_transmission: o.specular_transmission,
            thickness: o.thickness,
            ior: o.ior,
            alpha: o.alpha.clone(),
            alpha_cutoff: o.alpha_cutoff,
            double_sided: o.double_sided,
        }
    }

    /// One line for summaries, empty when nothing is set.
    pub fn describe(&self) -> String {
        let f = [
            ("clearcoat", self.clearcoat),
            ("clearcoat_roughness", self.clearcoat_roughness),
            ("anisotropy", self.anisotropy),
            ("anisotropy_rotation", self.anisotropy_rotation),
            ("transmission", self.transmission),
            ("specular_transmission", self.specular_transmission),
            ("thickness", self.thickness),
            ("ior", self.ior),
        ];
        let mut out: Vec<String> = f
            .iter()
            .filter_map(|(k, v)| v.map(|v| format!("{k} {v}")))
            .collect();
        if let Some(a) = &self.alpha {
            out.push(format!("alpha {a}"));
        }
        if let Some(c) = self.alpha_cutoff {
            out.push(format!("alpha_cutoff {c}"));
        }
        if let Some(d) = self.double_sided {
            out.push(format!("double_sided {d}"));
        }
        out.join(", ")
    }
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

/// One layer's channel values, per sample.
struct Layered {
    base: Vec<[f32; 3]>,
    rough: Vec<f32>,
    metal: Vec<f32>,
    normal: Vec<[f32; 3]>,
    height: Vec<f32>,
    ao: Vec<f32>,
    emis: Vec<[f32; 3]>,
    opac: Vec<f32>,
}

/// Assembles the channels from an evaluated graph.
pub fn channels(ev: &Evaluated, s: &Samples) -> Result<Channels, String> {
    let o = ev.output().cloned().unwrap_or_default();
    let mut driven = Vec::new();
    let l = assemble(ev, s, &o, "output", &mut driven, 0)?;
    driven.sort_unstable();
    driven.dedup();
    Ok(Channels {
        w: s.w,
        h: s.h,
        base_color: l.base,
        roughness: l.rough,
        metallic: l.metal,
        normal: l.normal,
        height: l.height,
        ao: l.ao,
        emissive: l.emis,
        opacity: l.opac,
        driven,
        extras: Extras::of(&o),
        runs: ev.runs,
    })
}

/// One output node's channels, with its `over` layers blended in.
fn assemble(
    ev: &Evaluated,
    s: &Samples,
    o: &node::Output,
    who: &str,
    driven: &mut Vec<&'static str>,
    depth: usize,
) -> Result<Layered, String> {
    if depth > 8 {
        return Err(format!("{who}: layers nest too deep (a layer cycle?)"));
    }
    let n = s.len();
    let mut ch = |name: &'static str, i: &Option<In>, default: Field| -> Result<Field, String> {
        match i {
            Some(i) => {
                driven.push(name);
                ev.field(i).map_err(|e| format!("{who}.{name}: {e}"))
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
    let mut acc = Layered {
        base: par3(&base),
        rough: par1(&rough),
        metal: par1(&metal),
        normal,
        height: par1(&height),
        ao: par1(&ao),
        emis: par3(&emis),
        opac: par1(&opac),
    };
    for (li, layer) in o.over.iter().enumerate() {
        let lw = format!("{who}.over[{li}]");
        let lo = ev
            .output_named(&layer.output)
            .ok_or_else(|| format!("{lw}: `{}` is not an output node", layer.output))?;
        let top = assemble(ev, s, lo, &layer.output, driven, depth + 1)?;
        let m = ev
            .field(&layer.mask)
            .map_err(|e| format!("{lw}.mask: {e}"))?;
        let t: Vec<f32> = (0..n)
            .into_par_iter()
            .map(|i| m.f(i).clamp(0.0, 1.0))
            .collect();
        let l1 = |a: &mut Vec<f32>, b: &[f32]| {
            a.par_iter_mut()
                .enumerate()
                .for_each(|(i, x)| *x += (b[i] - *x) * t[i]);
        };
        let l3 = |a: &mut Vec<[f32; 3]>, b: &[[f32; 3]]| {
            a.par_iter_mut().enumerate().for_each(|(i, x)| {
                for c in 0..3 {
                    x[c] += (b[i][c] - x[c]) * t[i];
                }
            });
        };
        l3(&mut acc.base, &top.base);
        l1(&mut acc.rough, &top.rough);
        l1(&mut acc.metal, &top.metal);
        l1(&mut acc.height, &top.height);
        l1(&mut acc.ao, &top.ao);
        l3(&mut acc.emis, &top.emis);
        l1(&mut acc.opac, &top.opac);
        l3(&mut acc.normal, &top.normal);
        acc.normal.par_iter_mut().for_each(|v| {
            let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            *v = if l > 1e-9 {
                v.map(|c| c / l)
            } else {
                [0.0, 0.0, 1.0]
            };
        });
    }
    Ok(acc)
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

    /// Share of texels (worst over the given channels) where this bake and
    /// `shifted` (the same graph baked half a tile over) disagree once
    /// `shifted` is rolled back by half a tile: 0 for a graph that is truly
    /// periodic on the tile, large for one that is not.
    pub fn periodic_mismatch(
        &self,
        shifted: &Channels,
        names: &[&str],
    ) -> Result<(f32, &'static str), String> {
        let (w, h) = (self.w, self.h);
        let mut worst = (0.0f32, "");
        for &name in CHANNELS.iter().filter(|c| names.contains(c)) {
            let (a, b) = (self.image(name)?, shifted.image(name)?);
            let bad = (0..w * h)
                .into_par_iter()
                .filter(|&i| {
                    let (x, y) = (i % w, i / w);
                    // shifted texel (x, y) sits at original (x + w/2, y + h/2).
                    let j = ((y + h / 2) % h) * w + (x + w / 2) % w;
                    (0..3).any(|k| (a.px[j * 4 + k] as i32 - b.px[i * 4 + k] as i32).abs() > 12)
                })
                .count();
            let f = bad as f32 / (w * h).max(1) as f32;
            if f > worst.0 {
                worst = (f, name);
            }
        }
        Ok(worst)
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
        let shifted = bake(&bricks(), &p, &Samples::tile_shifted(512, 1.0, 256), false)
            .unwrap_or_else(|e| panic!("{e}"));
        let (m, ch) = a
            .periodic_mismatch(&shifted, CHANNELS)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(m < 0.002, "{ch} mismatch {m}");
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
