//! Material nodes beyond the basics: `scatter` (a tile sampler: shapes
//! strewn with random position, rotation, scale and stacking), mesh-aware
//! masks (`mesh_ao`, `mesh_curvature`), `gradient`, `facing`, `image`,
//! `islands` and `dir_blur`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::Deserialize;

use super::meshvox;
use super::node::{Buf, Cx, Eval, Field, In, Outs, pf, pid, pv2, pv3, srgb_to_linear};
use super::noise;

#[inline]
fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

// ---------------------------------------------------------------- scatter

/// A tile sampler: instances of a shape strewn over a jittered grid of
/// `cell` meters (`per_cell` each), every one with a random offset,
/// rotation and scale, stacked by `layering`. The top instance at each
/// sample drives the ports, so any detail can be drawn inside it from
/// `local`:
/// - default / `mask`: coverage of the top instance (0..1, anti-aliased);
/// - `local`: coordinates inside it, -1..1 across its width (x) and from
///   base (-1) to tip (+1) along its axis (y);
/// - `id`: per-instance id (feed `random`);
/// - `height`: a 0..1 dome over the top instance (0 where none);
/// - `edge`: meters inside the top instance's outline.
///
/// Shapes: ellipse (default), circle, rect, feather (rounded tip, narrow
/// base), leaf (pointed both ends). The instance axis points along `angle`
/// (degrees, 0 = +v), or along `flow` (a vec2 direction field) when given.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Scatter {
    #[serde(default)]
    pub coords: Option<In>,
    /// Grid cell in meters (snapped to divide the tile).
    pub cell: f32,
    /// Instance size [width, length] in meters at scale 1.
    pub size: [f32; 2],
    #[serde(default)]
    pub shape: Option<String>,
    /// Instances per cell (default 1).
    #[serde(default)]
    pub per_cell: Option<u32>,
    /// Chance each instance exists, 0..1 (default 1).
    #[serde(default)]
    pub density: Option<f32>,
    /// Position jitter within the cell, 0..1 (default 1).
    #[serde(default)]
    pub jitter: Option<f32>,
    /// Scale range [min, max] (default [1, 1]).
    #[serde(default)]
    pub scale: Option<[f32; 2]>,
    /// Axis direction, degrees (0 = +v).
    #[serde(default)]
    pub angle: Option<f32>,
    /// Random rotation either way, degrees (180 = any direction).
    #[serde(default)]
    pub angle_jitter: Option<f32>,
    /// A vec2 direction field the axis follows (adds to `angle`).
    #[serde(default)]
    pub flow: Option<In>,
    /// "random" (default), or "flow": instances upstream (behind their tip
    /// direction) lie on top, like feathers, scales and shingles.
    #[serde(default)]
    pub layering: Option<String>,
    /// Edge softness in meters (default one texel).
    #[serde(default)]
    pub softness: Option<f32>,
    #[serde(default)]
    pub seed: Option<u32>,
}

struct Hit {
    depth: f32,
    cov: f32,
    local: [f32; 2],
    id: u32,
    inside: f32,
}

impl Eval for Scatter {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let c = cx.coords2(&self.coords)?;
        let flow = match &self.flow {
            Some(f) => Some(cx.input(f)?),
            None => None,
        };
        let shape = self.shape.as_deref().unwrap_or("ellipse");
        if !matches!(shape, "ellipse" | "circle" | "rect" | "feather" | "leaf") {
            return Err(format!(
                "shape: `{shape}`: use ellipse, circle, rect, feather or leaf"
            ));
        }
        let layering = self.layering.as_deref().unwrap_or("random");
        if !matches!(layering, "random" | "flow") {
            return Err(format!("layering: `{layering}`: use random or flow"));
        }
        if self.cell <= 0.0 || self.size[0] <= 0.0 || self.size[1] <= 0.0 {
            return Err("cell and size must be positive".into());
        }
        let period = cx.s.period;
        let cell = if period > 0.0 {
            period / (period / self.cell).round().max(1.0)
        } else {
            self.cell
        };
        let per = if period > 0.0 {
            (period / cell).round().max(1.0) as i32
        } else {
            0
        };
        let wrap = |v: i32| if per > 0 { v.rem_euclid(per) } else { v };
        let per_cell = self.per_cell.unwrap_or(1).clamp(1, 16) as i32;
        let density = self.density.unwrap_or(1.0);
        let jitter = self.jitter.unwrap_or(1.0).clamp(0.0, 1.0);
        let [s0, s1] = self.scale.unwrap_or([1.0, 1.0]);
        let a0 = self.angle.unwrap_or(0.0).to_radians();
        let aj = self.angle_jitter.unwrap_or(0.0).to_radians();
        let soft = self.softness.unwrap_or(0.0).max(cx.s.texel);
        let seed = self.seed.unwrap_or(0);
        let reach = self.size[0].max(self.size[1]) * s0.max(s1) * 0.5;
        let r = ((reach / cell) + jitter * 0.5).ceil().clamp(1.0, 4.0) as i32;
        let (hw, hl) = (self.size[0] * 0.5, self.size[1] * 0.5);
        let probe = |i: usize| -> Option<Hit> {
            let p = c.v2(i);
            let base = match &flow {
                Some(f) => {
                    let d = f.v2(i);
                    if d[0] * d[0] + d[1] * d[1] > 1e-12 {
                        (-d[0]).atan2(d[1])
                    } else {
                        0.0
                    }
                }
                None => 0.0,
            } + a0;
            let (gx, gy) = ((p[0] / cell).floor() as i32, (p[1] / cell).floor() as i32);
            let mut best: Option<Hit> = None;
            for oy in -r..=r {
                for ox in -r..=r {
                    let (cx_, cy) = (gx + ox, gy + oy);
                    let (wx, wy) = (wrap(cx_), wrap(cy));
                    for k in 0..per_cell {
                        let h = |j: i32| noise::unit(noise::hash3(wx, wy, k * 16 + j, seed));
                        if h(0) > density {
                            continue;
                        }
                        let ctr = [
                            (cx_ as f32 + 0.5 + (h(1) - 0.5) * jitter) * cell,
                            (cy as f32 + 0.5 + (h(2) - 0.5) * jitter) * cell,
                        ];
                        let s = s0 + (s1 - s0) * h(3);
                        let a = base + (h(4) * 2.0 - 1.0) * aj;
                        let (sn, cs) = a.sin_cos();
                        let d = [p[0] - ctr[0], p[1] - ctr[1]];
                        let lx = d[0] * cs + d[1] * sn;
                        let ly = -d[0] * sn + d[1] * cs;
                        let (nx, ny) = (lx / (hw * s), ly / (hl * s));
                        if nx.abs() > 1.5 || ny.abs() > 1.5 {
                            continue;
                        }
                        let sd = match shape {
                            "rect" => nx.abs().max(ny.abs()) - 1.0,
                            "feather" => {
                                let wf = 0.3 + 0.7 * ((ny + 1.0) * 0.5).clamp(0.0, 1.0).sqrt();
                                ((nx / wf).powi(2) + ny * ny).sqrt() - 1.0
                            }
                            "leaf" => (nx.abs() - (1.0 - ny * ny).max(0.0)).max(ny.abs() - 1.0),
                            _ => (nx * nx + ny * ny).sqrt() - 1.0,
                        };
                        let inside = -sd * hw.min(hl) * s;
                        let cov = smoothstep(-soft * 0.5, soft * 0.5, inside);
                        if cov <= 0.0 {
                            continue;
                        }
                        let depth = match layering {
                            "flow" => {
                                let tip = [-a.sin(), a.cos()];
                                -(d[0] * -tip[0] + d[1] * -tip[1]) / cell + h(5) * 1e-3
                            }
                            _ => h(5),
                        };
                        if best.as_ref().is_none_or(|b| depth > b.depth) {
                            best = Some(Hit {
                                depth,
                                cov,
                                local: [nx, ny],
                                id: noise::hash3(wx, wy, k, seed.wrapping_add(101)),
                                inside,
                            });
                        }
                    }
                }
            }
            best
        };
        let n = cx.n();
        let hits: Vec<Option<Hit>> = {
            use rayon::prelude::*;
            (0..n)
                .into_par_iter()
                .with_min_len(1024)
                .map(probe)
                .collect()
        };
        let m = |i: usize| hits[i].as_ref().map_or(0.0, |h| h.cov);
        Ok(vec![
            ("mask", pf(n, m)),
            (
                "local",
                pv2(n, |i| hits[i].as_ref().map_or([0.0, 0.0], |h| h.local)),
            ),
            ("id", pid(n, |i| hits[i].as_ref().map_or(0, |h| h.id))),
            (
                "height",
                pf(n, |i| {
                    hits[i].as_ref().map_or(0.0, |h| {
                        let r2 = h.local[0] * h.local[0] + h.local[1] * h.local[1];
                        h.cov * (1.0 - r2.min(1.0)).sqrt()
                    })
                }),
            ),
            (
                "edge",
                pf(n, |i| hits[i].as_ref().map_or(0.0, |h| h.inside.max(0.0))),
            ),
        ])
    }
}

// ---------------------------------------------------------------- mesh masks

/// Ambient occlusion from the part's own mesh (object domain): 1 open,
/// toward 0 in creases and hollows. `radius` meters (default 8% of the
/// part's size); 1 everywhere without a mesh (tile domain, the preview
/// sphere).
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MeshAo {
    #[serde(default)]
    pub radius: Option<f32>,
    /// Ray directions (default 24).
    #[serde(default)]
    pub samples: Option<u32>,
    /// 0..1 (default 1).
    #[serde(default)]
    pub strength: Option<f32>,
}

impl Eval for MeshAo {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let n = cx.n();
        let Some(mesh) = &cx.s.mesh else {
            return Ok(vec![("", Buf::F(vec![1.0; n]))]);
        };
        let g = mesh.grid();
        let r = self.radius.unwrap_or(g.extent() * 0.08).max(g.cell * 2.0);
        let dirs = meshvox::directions((self.samples.unwrap_or(24).clamp(4, 128) * 2) as usize);
        let k = self.strength.unwrap_or(1.0);
        let (pos, nrm) = (Field::Buf(cx.s.pos.clone()), Field::Buf(cx.s.nrm.clone()));
        let valid = cx.s.valid.clone();
        Ok(vec![(
            "",
            pf(n, |i| {
                if valid.as_ref().is_some_and(|v| !v[i]) {
                    return 1.0;
                }
                let (p, nn) = (pos.v3(i), nrm.v3(i));
                let start = [0, 1, 2].map(|a| p[a] + nn[a] * g.cell * 1.5);
                let (mut occ, mut wsum) = (0.0f32, 0.0f32);
                for d in &dirs {
                    let w = d[0] * nn[0] + d[1] * nn[1] + d[2] * nn[2];
                    if w <= 0.05 {
                        continue;
                    }
                    wsum += w;
                    for s in 1..=4 {
                        let t = r * s as f32 / 4.0;
                        if g.inside([0, 1, 2].map(|a| start[a] + d[a] * t)) {
                            occ += w;
                            break;
                        }
                    }
                }
                (1.0 - k * occ / wsum.max(1e-6)).clamp(0.0, 1.0)
            }),
        )])
    }
}

/// Curvature from the part's own mesh (object domain): the share of a
/// small ball around each point that is outside the solid. Default port:
/// 0..1 with 0.5 flat, above on convex edges, below in concave creases;
/// `convex` and `concave` are 0..1 masks (edge wear, dirt in crevices).
/// `radius` meters (default 3% of the part's size); flat without a mesh.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MeshCurvature {
    #[serde(default)]
    pub radius: Option<f32>,
    /// Contrast (default 1).
    #[serde(default)]
    pub gain: Option<f32>,
}

impl Eval for MeshCurvature {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let n = cx.n();
        let Some(mesh) = &cx.s.mesh else {
            return Ok(vec![
                ("", Buf::F(vec![0.5; n])),
                ("convex", Buf::F(vec![0.0; n])),
                ("concave", Buf::F(vec![0.0; n])),
            ]);
        };
        let g = mesh.grid();
        let r = self.radius.unwrap_or(g.extent() * 0.03).max(g.cell * 1.5);
        let gain = self.gain.unwrap_or(1.0);
        let dirs = meshvox::directions(48);
        let pos = Field::Buf(cx.s.pos.clone());
        let valid = cx.s.valid.clone();
        let conv: Vec<f32> = {
            use rayon::prelude::*;
            (0..n)
                .into_par_iter()
                .with_min_len(1024)
                .map(|i| {
                    if valid.as_ref().is_some_and(|v| !v[i]) {
                        return 0.0;
                    }
                    let p = pos.v3(i);
                    let mut inside = 0usize;
                    let mut total = 0usize;
                    for d in &dirs {
                        for f in [0.5f32, 1.0] {
                            total += 1;
                            if g.inside([0, 1, 2].map(|a| p[a] + d[a] * r * f)) {
                                inside += 1;
                            }
                        }
                    }
                    ((0.5 - inside as f32 / total as f32) * 2.0 * gain).clamp(-1.0, 1.0)
                })
                .collect()
        };
        Ok(vec![
            ("", pf(n, |i| 0.5 + 0.5 * conv[i])),
            ("convex", pf(n, |i| conv[i].max(0.0))),
            ("concave", pf(n, |i| (-conv[i]).max(0.0))),
        ])
    }
}

// ---------------------------------------------------------------- gradients

/// A 0..1 ramp from `from` to `to`: `linear` (default; along the line) or
/// `radial` (distance from `from`, 1 at |to - from|). Points with three
/// values read object-space position (a fade across a part); two read uv.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Gradient {
    #[serde(default)]
    pub coords: Option<In>,
    pub from: Vec<f32>,
    pub to: Vec<f32>,
    #[serde(default)]
    pub mode: Option<String>,
}

impl Eval for Gradient {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let dims = self.from.len();
        if !(2..=3).contains(&dims) || self.to.len() != dims {
            return Err("from and to need 2 (uv) or 3 (position) values each".into());
        }
        let c = match (&self.coords, dims) {
            (Some(c), _) => cx.input(c)?,
            (None, 3) => Field::Buf(cx.s.pos.clone()),
            (None, _) => Field::Buf(cx.s.uv.clone()),
        };
        let radial = match self.mode.as_deref().unwrap_or("linear") {
            "linear" => false,
            "radial" => true,
            o => return Err(format!("mode: `{o}`: use linear or radial")),
        };
        let (a, b) = (self.from.clone(), self.to.clone());
        let ab: Vec<f32> = (0..dims).map(|k| b[k] - a[k]).collect();
        let len2 = ab.iter().map(|x| x * x).sum::<f32>().max(1e-12);
        Ok(vec![(
            "",
            pf(cx.n(), |i| {
                let p = c.v3(i);
                let d: Vec<f32> = (0..dims).map(|k| p[k] - a[k]).collect();
                if radial {
                    (d.iter().map(|x| x * x).sum::<f32>() / len2)
                        .sqrt()
                        .min(1.0)
                } else {
                    (d.iter().zip(&ab).map(|(x, y)| x * y).sum::<f32>() / len2).clamp(0.0, 1.0)
                }
            }),
        )])
    }
}

/// How much the surface faces `dir` (default up, [0, 1, 0]; object space):
/// the cosine, smoothstepped from `lo` (default 0) to `hi` (default 1).
/// Dust and snow on tops, moss on north faces, drips on undersides.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Facing {
    #[serde(default)]
    pub dir: Option<[f32; 3]>,
    #[serde(default)]
    pub normal: Option<In>,
    #[serde(default)]
    pub lo: Option<f32>,
    #[serde(default)]
    pub hi: Option<f32>,
}

impl Eval for Facing {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let nrm = cx.or(&self.normal, Field::Buf(cx.s.nrm.clone()))?;
        let d = self.dir.unwrap_or([0.0, 1.0, 0.0]);
        let l = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1e-9);
        let d = d.map(|x| x / l);
        let (lo, hi) = (self.lo.unwrap_or(0.0), self.hi.unwrap_or(1.0));
        Ok(vec![(
            "",
            pf(cx.n(), |i| {
                let v = nrm.v3(i);
                smoothstep(lo, hi, v[0] * d[0] + v[1] * d[1] + v[2] * d[2])
            }),
        )])
    }
}

// ---------------------------------------------------------------- image

/// A PNG laid on the surface: `size` [w, h] meters per repeat (default
/// [1, 1]), tiled. Ports: default colour (linear), `alpha`, `value`
/// (luminance). The path is absolute or relative to the server's working
/// directory; the file is read once per path and modification time.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Image {
    pub path: String,
    #[serde(default)]
    pub coords: Option<In>,
    #[serde(default)]
    pub size: Option<[f32; 2]>,
}

struct Img {
    w: usize,
    h: usize,
    /// Linear rgb + alpha.
    px: Vec<[f32; 4]>,
}

type ImgCache = Mutex<Option<HashMap<(String, u128), Arc<Img>>>>;
static IMAGES: ImgCache = Mutex::new(None);

fn load_image(path: &str) -> Result<Arc<Img>, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("path `{path}`: {e}"))?;
    let stamp = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    let key = (path.to_string(), stamp);
    if let Ok(g) = IMAGES.lock()
        && let Some(i) = g.as_ref().and_then(|m| m.get(&key))
    {
        return Ok(i.clone());
    }
    let bytes = std::fs::read(path).map_err(|e| format!("path `{path}`: {e}"))?;
    let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec
        .read_info()
        .map_err(|e| format!("`{path}`: not a PNG: {e}"))?;
    let mut buf = vec![0; reader.output_buffer_size().ok_or("png: size overflow")?];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("`{path}`: {e}"))?;
    let ch = match info.color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => return Err(format!("`{path}`: indexed PNG not expanded")),
    };
    let (w, h) = (info.width as usize, info.height as usize);
    let px = buf[..info.buffer_size()]
        .chunks(ch)
        .map(|p| {
            let f = |v: u8| v as f32 / 255.0;
            match ch {
                1 => [srgb_to_linear(f(p[0])); 3]
                    .into_iter()
                    .chain([1.0])
                    .collect(),
                2 => [srgb_to_linear(f(p[0])); 3]
                    .into_iter()
                    .chain([f(p[1])])
                    .collect(),
                3 => [f(p[0]), f(p[1]), f(p[2])]
                    .map(srgb_to_linear)
                    .into_iter()
                    .chain([1.0])
                    .collect(),
                _ => [f(p[0]), f(p[1]), f(p[2])]
                    .map(srgb_to_linear)
                    .into_iter()
                    .chain([f(p[3])])
                    .collect::<Vec<f32>>(),
            }
        })
        .map(|v: Vec<f32>| [v[0], v[1], v[2], v[3]])
        .collect();
    let img = Arc::new(Img { w, h, px });
    if let Ok(mut g) = IMAGES.lock() {
        let m = g.get_or_insert_with(HashMap::new);
        if m.len() > 32 {
            m.clear();
        }
        m.insert(key, img.clone());
    }
    Ok(img)
}

impl Eval for Image {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let img = load_image(&self.path)?;
        let c = cx.coords2(&self.coords)?;
        let [sw, sh] = self.size.unwrap_or([1.0, 1.0]);
        if sw <= 0.0 || sh <= 0.0 {
            return Err("size must be positive".into());
        }
        let sample = |i: usize| -> [f32; 4] {
            let p = c.v2(i);
            let fx = (p[0] / sw).rem_euclid(1.0) * img.w as f32 - 0.5;
            let fy = (p[1] / sh).rem_euclid(1.0) * img.h as f32 - 0.5;
            let (x0, y0) = (fx.floor(), fy.floor());
            let (tx, ty) = (fx - x0, fy - y0);
            let at = |x: f32, y: f32| {
                let xi = (x as i64).rem_euclid(img.w as i64) as usize;
                let yi = (y as i64).rem_euclid(img.h as i64) as usize;
                img.px[yi * img.w + xi]
            };
            let (a, b, cc, d) = (
                at(x0, y0),
                at(x0 + 1.0, y0),
                at(x0, y0 + 1.0),
                at(x0 + 1.0, y0 + 1.0),
            );
            [0, 1, 2, 3].map(|k| {
                let top = a[k] + (b[k] - a[k]) * tx;
                let bot = cc[k] + (d[k] - cc[k]) * tx;
                top + (bot - top) * ty
            })
        };
        let n = cx.n();
        let px: Vec<[f32; 4]> = {
            use rayon::prelude::*;
            (0..n)
                .into_par_iter()
                .with_min_len(2048)
                .map(sample)
                .collect()
        };
        Ok(vec![
            ("", pv3(n, |i| [px[i][0], px[i][1], px[i][2]])),
            ("alpha", pf(n, |i| px[i][3])),
            (
                "value",
                pf(n, |i| {
                    0.2126 * px[i][0] + 0.7152 * px[i][1] + 0.0722 * px[i][2]
                }),
            ),
        ])
    }
}

// ---------------------------------------------------------------- islands

/// Connected regions of a mask (above `threshold`, default 0.5) on the bake
/// grid, wrapping in the tile domain. Ports: default `id` (one per island,
/// 0 outside; feed `random` for a value per stone or plank) and `area`
/// (the island's area in m²).
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Islands {
    pub input: In,
    #[serde(default)]
    pub threshold: Option<f32>,
}

impl Eval for Islands {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let m = cx.input(&self.input)?;
        let t = self.threshold.unwrap_or(0.5);
        let s = cx.s;
        let n = cx.n();
        let on: Vec<bool> = (0..n).map(|i| m.f(i) > t).collect();
        let mut parent: Vec<usize> = (0..n).collect();
        fn find(p: &mut [usize], mut i: usize) -> usize {
            while p[i] != i {
                p[i] = p[p[i]];
                i = p[i];
            }
            i
        }
        for i in 0..n {
            if !on[i] {
                continue;
            }
            for (dx, dy) in [(1, 0), (0, 1)] {
                if let Some(j) = s.neighbour(i, dx, dy)
                    && on[j]
                {
                    let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                    if a != b {
                        parent[a.max(b)] = a.min(b);
                    }
                }
            }
        }
        let roots: Vec<usize> = (0..n).map(|i| find(&mut parent, i)).collect();
        let mut count: HashMap<usize, usize> = HashMap::new();
        for i in 0..n {
            if on[i] {
                *count.entry(roots[i]).or_default() += 1;
            }
        }
        let texel2 = s.texel * s.texel;
        Ok(vec![
            (
                "id",
                pid(n, |i| {
                    if on[i] {
                        noise::hash3(roots[i] as i32, 7, 3, 55) | 1
                    } else {
                        0
                    }
                }),
            ),
            (
                "area",
                pf(n, |i| {
                    if on[i] {
                        count.get(&roots[i]).copied().unwrap_or(0) as f32 * texel2
                    } else {
                        0.0
                    }
                }),
            ),
        ])
    }
}

// ---------------------------------------------------------------- dir blur

/// Smears `input` along a direction: `angle` degrees (0 = +u), or a vec2
/// `flow` field, over `length` meters; `mode` both (default) or forward.
/// Streaks, rain runs, brushed metal, hair.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DirBlur {
    pub input: In,
    pub length: f32,
    #[serde(default)]
    pub angle: Option<f32>,
    #[serde(default)]
    pub flow: Option<In>,
    #[serde(default)]
    pub mode: Option<String>,
}

impl Eval for DirBlur {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let a = cx.input(&self.input)?;
        let flow = match &self.flow {
            Some(f) => Some(cx.input(f)?),
            None => None,
        };
        let fwd = match self.mode.as_deref().unwrap_or("both") {
            "both" => false,
            "forward" => true,
            o => return Err(format!("mode: `{o}`: use both or forward")),
        };
        let s = cx.s;
        let steps = (self.length / s.texel).round().clamp(1.0, 128.0) as i64;
        let ang = self.angle.unwrap_or(0.0).to_radians();
        let dir = |i: usize| -> [f32; 2] {
            match &flow {
                Some(f) => {
                    let d = f.v2(i);
                    let l = (d[0] * d[0] + d[1] * d[1]).sqrt();
                    if l > 1e-9 {
                        [d[0] / l, d[1] / l]
                    } else {
                        [1.0, 0.0]
                    }
                }
                None => [ang.cos(), ang.sin()],
            }
        };
        let avg = |i: usize, get: &dyn Fn(usize) -> [f32; 3]| {
            let d = dir(i);
            let (mut acc, mut cnt) = ([0.0f32; 3], 0.0f32);
            let lo = if fwd { 0 } else { -steps };
            for k in lo..=steps {
                let (dx, dy) = (
                    (d[0] * k as f32).round() as i64,
                    (d[1] * k as f32).round() as i64,
                );
                if let Some(j) = s.neighbour(i, dx, dy) {
                    let v = get(j);
                    acc = [acc[0] + v[0], acc[1] + v[1], acc[2] + v[2]];
                    cnt += 1.0;
                }
            }
            acc.map(|x| x / cnt.max(1.0))
        };
        let n = cx.n();
        Ok(vec![(
            "",
            if a.dims() == 1 {
                pf(n, |i| avg(i, &|j| [a.f(j); 3])[0])
            } else {
                pv3(n, |i| avg(i, &|j| a.v3(j)))
            },
        )])
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use crate::material::bake::{CHANNELS, Channels, bake};
    use crate::material::eval::Samples;
    use crate::params::Params;

    fn run(nodes: Value, s: &Samples) -> Channels {
        let Value::Array(n) = nodes else {
            panic!("nodes must be an array");
        };
        bake(&n, &Params::new(), s, false).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn scatter_is_tileable_and_covers_with_varied_instances() {
        let g = json!([
            {"type": "scatter", "name": "sc", "cell": 0.02, "size": [0.02, 0.03], "shape": "feather",
             "per_cell": 2, "scale": [0.8, 1.2], "angle_jitter": 15, "layering": "flow"},
            {"type": "random", "name": "tint", "id": "sc.id", "out": "color"},
            {"type": "blend", "name": "col", "a": "#202020", "b": "tint", "opacity": "sc.mask"},
            {"type": "remap", "name": "h", "input": "sc.height", "from": [0, 1], "to": [0, 0.0005]},
            {"type": "output", "name": "out", "base_color": "col", "height": "h"}
        ]);
        let a = run(g.clone(), &Samples::tile(256, 0.2));
        let b = run(g, &Samples::tile_shifted(256, 0.2, 128));
        let (m, ch) = a
            .periodic_mismatch(&b, CHANNELS)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(m < 0.002, "{ch} mismatch {m}");
        let dark = a.base_color.iter().filter(|c| c[0] < 0.02).count();
        let frac = dark as f32 / a.base_color.len() as f32;
        assert!(frac < 0.2, "uncovered share {frac}");
        let mut distinct: Vec<[u32; 3]> = a
            .base_color
            .iter()
            .map(|c| c.map(|x| (x * 50.0) as u32))
            .collect();
        distinct.sort_unstable();
        distinct.dedup();
        assert!(distinct.len() > 40, "only {} tints", distinct.len());
    }

    #[test]
    fn layers_blend_whole_materials_by_mask() {
        let g = json!([
            {"type": "gradient", "name": "g", "from": [0, 0], "to": [1, 0]},
            {"type": "output", "name": "dirt", "base_color": "#ff0000", "roughness": 0.9},
            {"type": "output", "name": "out", "base_color": "#0000ff", "roughness": 0.2,
             "over": [{"output": "dirt", "mask": "g"}], "clearcoat": 0.5}
        ]);
        let c = run(g, &Samples::tile(64, 1.0));
        let (l, r) = (c.base_color[0], c.base_color[63]);
        assert!(l[2] > 0.9 && l[0] < 0.1, "left is the base: {l:?}");
        assert!(r[0] > 0.9 && r[2] < 0.1, "right is the layer: {r:?}");
        assert!(c.roughness[0] < 0.25 && c.roughness[63] > 0.85);
        assert_eq!(c.extras.clearcoat, Some(0.5));
    }

    #[test]
    fn mesh_masks_read_the_part() {
        let mut part =
            crate::primitives::primitive(crate::PrimitiveKind::Box, [0.2, 0.2, 0.2], "b")
                .unwrap_or_else(|e| panic!("{e}"));
        crate::material::atlas::box_atlas(&mut part, 128);
        let s = crate::material::atlas::samples(&part, 128);
        let g = json!([
            {"type": "mesh_ao", "name": "ao"},
            {"type": "mesh_curvature", "name": "cv", "radius": 0.01},
            {"type": "output", "name": "out", "ao": "ao", "roughness": "cv.convex", "metallic": "cv.concave"}
        ]);
        let c = run(g, &s);
        let valid: Vec<usize> = s
            .valid
            .as_ref()
            .map(|v| (0..v.len()).filter(|&i| v[i]).collect())
            .unwrap_or_default();
        assert!(!valid.is_empty());
        let mean_ao = valid.iter().map(|&i| c.ao[i]).sum::<f32>() / valid.len() as f32;
        assert!(mean_ao > 0.9, "a cube is open everywhere: {mean_ao}");
        let edge = valid.iter().map(|&i| c.roughness[i]).fold(0.0f32, f32::max);
        assert!(edge > 0.3, "cube edges read convex: {edge}");
        let concave = valid.iter().map(|&i| c.metallic[i]).fold(0.0f32, f32::max);
        assert!(concave < 0.2, "a cube has no creases: {concave}");
        // Without a mesh (tile domain) they are neutral.
        let t = run(
            json!([{"type": "mesh_ao", "name": "ao"}, {"type": "output", "name": "o", "ao": "ao"}]),
            &Samples::tile(16, 1.0),
        );
        assert!(t.ao.iter().all(|&a| a == 1.0));
    }

    #[test]
    fn islands_image_facing_and_dir_blur() {
        let g = json!([
            {"type": "tile_grid", "name": "cells", "size": [0.25, 0.25]},
            {"type": "circle", "name": "c", "coords": "cells.local", "radius": 0.08},
            {"type": "mask", "name": "m", "sdf": "c"},
            {"type": "islands", "name": "isl", "input": "m"},
            {"type": "random", "name": "r", "id": "isl.id"},
            {"type": "dir_blur", "name": "streak", "input": "m", "length": 0.05, "angle": 90},
            {"type": "facing", "name": "up", "dir": [0, 0, 1]},
            {"type": "output", "name": "o", "roughness": "r", "metallic": "streak", "ao": "up"}
        ]);
        let c = run(g, &Samples::tile(128, 1.0));
        let mut vals: Vec<u32> = c.roughness.iter().map(|x| (x * 1e4) as u32).collect();
        vals.sort_unstable();
        vals.dedup();
        assert!(
            vals.len() >= 10,
            "16 islands give distinct values: {}",
            vals.len()
        );
        assert!(c.ao.iter().all(|&a| a > 0.99), "tile normal faces +z");
        let soft = c.metallic.iter().filter(|&&m| m > 0.05 && m < 0.95).count();
        assert!(soft > 500, "the blur smears edges");
        // A PNG on the surface.
        let path = std::env::temp_dir().join(format!("modeler_img_{}.png", std::process::id()));
        let png = crate::raster::encode_png(2, 2, &[255, 0, 0, 255].repeat(4))
            .unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(&path, png).unwrap_or_else(|e| panic!("{e}"));
        let c = run(
            json!([
                {"type": "image", "name": "img", "path": path.to_string_lossy(), "size": [0.5, 0.5]},
                {"type": "output", "name": "o", "base_color": "img", "roughness": "img.alpha"}
            ]),
            &Samples::tile(16, 1.0),
        );
        assert!(c.base_color.iter().all(|p| p[0] > 0.99 && p[1] < 0.01));
        assert!(c.roughness.iter().all(|&r| r > 0.99));
    }
}
