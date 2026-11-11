//! Material documents: a named node graph with its own history tree, the
//! session's working set of them, the deterministic checks and a headless
//! (CPU) preview image.

use std::collections::BTreeMap;
use std::time::Instant;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::bake::{self, CHANNELS, Channels};
use super::eval::{self, Samples};
use super::node::{self, Buf};
use crate::catalog::{self, Kind};
use crate::params::Params;
use crate::raster::Canvas;
use crate::{Doc, History};

/// On-disk schema version of a material document.
pub const MAT_FORMAT_VERSION: u32 = 1;

/// A procedural material: a graph of nodes ending in an `output` node.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MatDoc {
    pub name: String,
    #[serde(default)]
    pub format_version: u32,
    /// "tile" (a seamless square `tile` meters wide, mapped onto parts by
    /// their metric uvs) or "object" (evaluated at points on the surface:
    /// 3D noise and SDFs wrap corners with no seams).
    #[serde(default = "tile_domain")]
    pub domain: String,
    /// Tile size in meters (tile domain).
    #[serde(default = "one")]
    pub tile: f32,
    /// Bake resolution (texels along the tile, or per meter-ish on parts).
    #[serde(default = "res")]
    pub resolution: u32,
    #[serde(default)]
    pub nodes: Vec<Value>,
    #[serde(default)]
    pub params: Params,
    #[serde(default)]
    pub version: u64,
    #[serde(default, skip_serializing_if = "History::is_empty")]
    pub history: History<MatDoc>,
}

fn tile_domain() -> String {
    "tile".into()
}
fn one() -> f32 {
    1.0
}
fn res() -> u32 {
    512
}

impl Default for MatDoc {
    fn default() -> Self {
        MatDoc {
            name: "material".into(),
            format_version: MAT_FORMAT_VERSION,
            domain: tile_domain(),
            tile: 1.0,
            resolution: 512,
            nodes: Vec::new(),
            params: Params::new(),
            version: 0,
            history: History::default(),
        }
    }
}

impl Doc for MatDoc {
    fn trim(&mut self) {
        self.history = History::default();
    }
    fn version(&self) -> u64 {
        self.version
    }
}

/// Deterministic checks on a material (`material_ops` `expect`).
#[derive(Clone, Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MatExpect {
    /// The tile repeats with no seam (tile domain): the jump across each
    /// wrap edge is no bigger than the largest jump inside.
    #[serde(default)]
    pub tileable: Option<bool>,
    /// No NaN; roughness, metallic, ao, opacity and base colour in 0..1.
    #[serde(default)]
    pub ranges: Option<bool>,
    /// Non-metal base colour luminance stays in [lo, hi] (true = the
    /// physically plausible [0.02, 0.9]), at the 1st and 99th percentile.
    #[serde(default)]
    pub albedo: Option<Albedo>,
    /// Normals are unit length and face out (z > 0).
    #[serde(default)]
    pub normals: Option<bool>,
    /// Area fractions: [{"node": "brick_mask", "min": 0.8, "max": 0.9}]
    /// (share of samples where the node's value is above 0.5).
    #[serde(default)]
    pub coverage: Option<Vec<Coverage>>,
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum Albedo {
    On(bool),
    Range([f32; 2]),
}

#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Coverage {
    pub node: String,
    #[serde(default)]
    pub min: Option<f32>,
    #[serde(default)]
    pub max: Option<f32>,
}

impl MatDoc {
    pub fn named(name: &str) -> MatDoc {
        MatDoc {
            name: name.into(),
            ..MatDoc::default()
        }
    }

    pub fn check_format(&self) -> Result<(), String> {
        if self.format_version > MAT_FORMAT_VERSION {
            return Err(format!(
                "material `{}` has format_version {} but this build reads up to {MAT_FORMAT_VERSION}",
                self.name, self.format_version
            ));
        }
        Ok(())
    }

    /// The sample set this material bakes on at `n` texels: the tile, or (object
    /// domain, until part atlases) a preview sphere of `radius` meters.
    pub fn samples(&self, n: usize, radius: f32) -> Samples {
        if self.domain == "object" {
            Samples::sphere(n, radius)
        } else {
            Samples::tile(n, self.tile)
        }
    }

    pub fn bake(&self, n: usize, cached: bool) -> Result<Channels, String> {
        bake::bake(&self.nodes, &self.params, &self.samples(n, 0.25), cached)
    }

    fn node_name(v: &Value) -> Option<&str> {
        v.get("name").and_then(Value::as_str)
    }

    /// Applies a batch of node upserts (and `{"type": "delete", "name"}`)
    /// atomically: on any error nothing changes. Returns the report.
    pub fn apply(
        &mut self,
        items: Vec<Value>,
        params: Option<Params>,
        domain: Option<String>,
        tile: Option<f32>,
        resolution: Option<u32>,
        expect: Option<&MatExpect>,
    ) -> Result<(String, bool), String> {
        let started = Instant::now();
        let mut next = self.clone();
        next.history = History::default();
        if let Some(d) = domain {
            if d != "tile" && d != "object" {
                return Err(format!("domain: `{d}`: use \"tile\" or \"object\""));
            }
            next.domain = d;
        }
        if let Some(t) = tile {
            if !(t > 0.0 && t.is_finite()) {
                return Err(format!("tile: {t}: give a size in meters > 0"));
            }
            next.tile = t;
        }
        if let Some(r) = resolution {
            if !(16..=4096).contains(&r) {
                return Err(format!("resolution: {r}: use 16..4096"));
            }
            next.resolution = r;
        }
        if let Some(p) = params {
            next.params.extend(p);
        }
        let (mut added, mut changed, mut removed) = (Vec::new(), Vec::new(), Vec::new());
        for (k, item) in items.into_iter().enumerate() {
            let ty = item
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let Some(name) = Self::node_name(&item).map(str::to_string) else {
                return Err(format!("node #{k} ({ty}): missing \"name\""));
            };
            let at = next
                .nodes
                .iter()
                .position(|n| Self::node_name(n) == Some(&name));
            if ty == "delete" {
                let Some(at) = at else {
                    return Err(format!("node #{k} (delete): no node named `{name}`"));
                };
                next.nodes.remove(at);
                removed.push(name);
                continue;
            }
            match at {
                Some(at) => {
                    let old_ty = next.nodes[at]
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if old_ty != ty && !ty.is_empty() {
                        return Err(format!(
                            "node #{k} ({ty} \"{name}\"): `{name}` is a {old_ty}; delete it first or pick another name"
                        ));
                    }
                    if next.nodes[at] != item {
                        changed.push(name);
                        next.nodes[at] = item;
                    }
                }
                None => {
                    added.push(name);
                    next.nodes.push(item);
                }
            }
        }
        // Parse everything, then bake for real: kernel errors (a polygon with
        // two points) surface now, not at render time.
        let parsed = eval::parse(&next.nodes, &next.params)?;
        let ch = next.bake(next.resolution as usize, true)?;
        let lv = eval::levels(&parsed)?;
        next.version = self.version + 1;
        next.format_version = MAT_FORMAT_VERSION;
        let label = {
            let mut l: Vec<String> = Vec::new();
            l.extend(added.iter().map(|n| format!("+{n}")));
            l.extend(changed.iter().map(|n| format!("~{n}")));
            l.extend(removed.iter().map(|n| format!("-{n}")));
            if l.is_empty() {
                "material settings".to_string()
            } else {
                l.join(" ")
            }
        };
        let mut history = std::mem::take(&mut self.history);
        history.record(next.clone(), label);
        *self = next;
        self.history = history;
        let mut out = format!(
            "material `{}` v{} ({} domain{}, {}²): {} node(s) in {} level(s); {} computed, rest cached; {:.0} ms\n",
            self.name,
            self.version,
            self.domain,
            if self.domain == "tile" {
                format!(" {} m tile", self.tile)
            } else {
                String::new()
            },
            self.resolution,
            parsed.len(),
            lv.len(),
            ch.runs,
            started.elapsed().as_secs_f64() * 1000.0
        );
        for (sign, list) in [("+", &added), ("~", &changed), ("-", &removed)] {
            if !list.is_empty() {
                out += &format!("{sign} {}\n", list.join(", "));
            }
        }
        if !parsed.iter().any(|p| p.ty == "output") {
            out += "WARNING no output node: every channel is a default (add {\"type\": \"output\", \"name\": \"out\", \"base_color\": ...})\n";
        }
        out += &channel_ranges(&ch);
        let mut ok = true;
        if let Some(e) = expect {
            let checks = self.check(e, &ch)?;
            ok = checks.iter().all(|c| c.1);
            out += "\nexpect:\n";
            for (name, pass, why) in checks {
                out += &format!("{} {name}: {why}\n", if pass { "PASS" } else { "FAIL" });
            }
        }
        Ok((out, ok))
    }

    /// Runs the checks against a bake.
    pub fn check(
        &self,
        e: &MatExpect,
        ch: &Channels,
    ) -> Result<Vec<(String, bool, String)>, String> {
        let mut out = Vec::new();
        if e.tileable == Some(true) {
            if self.domain != "tile" {
                out.push((
                    "tileable".into(),
                    false,
                    "an object-domain material is not a tile".into(),
                ));
            } else {
                let mut worst = (0.0f32, "");
                for &c in CHANNELS {
                    if c == "height" && !ch.driven.contains(&"height") {
                        continue;
                    }
                    let r = ch.seam_ratio(c)?;
                    if r > worst.0 {
                        worst = (r, c);
                    }
                }
                let pass = worst.0 <= 2.0;
                out.push((
                    "tileable".into(),
                    pass,
                    if pass {
                        "every channel wraps with no seam".into()
                    } else {
                        format!(
                            "`{}` steps {:.1}x more across the tile edge than inside (median): a node is not periodic \
                             (rotated or non-integer-scaled coords, or a size that does not divide the tile)",
                            worst.1, worst.0
                        )
                    },
                ));
            }
        }
        if e.ranges == Some(true) {
            let mut bad = Vec::new();
            let scal = [
                ("roughness", &ch.roughness),
                ("metallic", &ch.metallic),
                ("ao", &ch.ao),
                ("opacity", &ch.opacity),
            ];
            for (n, v) in scal {
                let nan = v.iter().filter(|x| !x.is_finite()).count();
                let out_of = v.iter().filter(|x| !(-1e-4..=1.0001).contains(*x)).count();
                if nan + out_of > 0 {
                    bad.push(format!("{n}: {nan} NaN, {out_of} outside 0..1"));
                }
            }
            for (n, v) in [("base_color", &ch.base_color), ("emissive", &ch.emissive)] {
                let nan = v
                    .iter()
                    .filter(|c| c.iter().any(|x| !x.is_finite()))
                    .count();
                let out_of = if n == "base_color" {
                    v.iter()
                        .filter(|c| c.iter().any(|x| !(-1e-4..=1.0001).contains(x)))
                        .count()
                } else {
                    0
                };
                if nan + out_of > 0 {
                    bad.push(format!("{n}: {nan} NaN, {out_of} outside 0..1"));
                }
            }
            out.push((
                "ranges".into(),
                bad.is_empty(),
                if bad.is_empty() {
                    "all channels finite and in range".into()
                } else {
                    bad.join("; ")
                },
            ));
        }
        if let Some(a) = &e.albedo {
            let range = match a {
                Albedo::On(false) => None,
                Albedo::On(true) => Some([0.02, 0.9]),
                Albedo::Range(r) => Some(*r),
            };
            if let Some([lo, hi]) = range {
                let mut l: Vec<f32> = ch
                    .base_color
                    .iter()
                    .zip(&ch.metallic)
                    .filter(|(_, m)| **m < 0.5)
                    .map(|(c, _)| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2])
                    .collect();
                if l.is_empty() {
                    out.push(("albedo".into(), true, "every sample is metal".into()));
                } else {
                    l.sort_by(f32::total_cmp);
                    let p = |q: f32| l[((l.len() - 1) as f32 * q) as usize];
                    let (p1, p99) = (p(0.01), p(0.99));
                    let pass = p1 >= lo && p99 <= hi;
                    out.push((
                        "albedo".into(),
                        pass,
                        format!(
                            "luminance 1st..99th percentile {p1:.3}..{p99:.3} (want {lo}..{hi}{})",
                            if pass {
                                ""
                            } else if p1 < lo {
                                ": too dark, nothing real is that black"
                            } else {
                                ": too bright for a non-metal"
                            }
                        ),
                    ));
                }
            }
        }
        if e.normals == Some(true) {
            let bad = ch
                .normal
                .iter()
                .filter(|n| {
                    let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
                    !((l - 1.0).abs() < 1e-3 && n[2] > 0.0)
                })
                .count();
            out.push((
                "normals".into(),
                bad == 0,
                if bad == 0 {
                    "unit length, facing out".into()
                } else {
                    format!("{bad} normals not unit or facing in")
                },
            ));
        }
        if let Some(cov) = &e.coverage {
            let s = self.samples(self.resolution as usize, 0.25);
            let ev = eval::evaluate(&self.nodes, &self.params, &s, true)?;
            for c in cov {
                let f = ev
                    .field(&node::In::Name(c.node.clone()))
                    .map_err(|e| format!("coverage: {e}"))?;
                let n = s.len();
                let inside = (0..n).into_par_iter().filter(|&i| f.f(i) > 0.5).count();
                let frac = inside as f32 / n.max(1) as f32;
                let (lo, hi) = (c.min.unwrap_or(0.0), c.max.unwrap_or(1.0));
                out.push((
                    format!("coverage {}", c.node),
                    (lo..=hi).contains(&frac),
                    format!(
                        "{:.1}% of the area (want {:.1}..{:.1}%)",
                        frac * 100.0,
                        lo * 100.0,
                        hi * 100.0
                    ),
                ));
            }
        }
        Ok(out)
    }

    /// One line per node: name, type and its fields.
    pub fn describe(&self) -> String {
        let mut out = format!(
            "material `{}` v{}: {} domain{}, resolution {}\n",
            self.name,
            self.version,
            self.domain,
            if self.domain == "tile" {
                format!(", {} m tile", self.tile)
            } else {
                String::new()
            },
            self.resolution
        );
        if !self.params.is_empty() {
            let p: Vec<String> = self
                .params
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            out += &format!("params: {}\n", p.join(", "));
        }
        for n in &self.nodes {
            let mut body = n.clone();
            let (ty, name) = match body.as_object_mut() {
                Some(m) => (
                    m.remove("type")
                        .and_then(|v| v.as_str().map(str::to_string))
                        .unwrap_or_default(),
                    m.remove("name")
                        .and_then(|v| v.as_str().map(str::to_string))
                        .unwrap_or_default(),
                ),
                None => Default::default(),
            };
            out += &format!("{name} = {ty} {body}\n");
        }
        out
    }
}

/// Min..max (or mean colour) of each channel the graph drives.
pub fn channel_ranges(ch: &Channels) -> String {
    let mut out = String::from("channels:");
    let span = |v: &[f32]| {
        v.iter()
            .fold((f32::MAX, f32::MIN), |(a, b), &x| (a.min(x), b.max(x)))
    };
    for &c in CHANNELS {
        if !ch.driven.contains(&c) {
            continue;
        }
        let txt = match c {
            "base_color" | "emissive" => {
                let v = if c == "base_color" {
                    &ch.base_color
                } else {
                    &ch.emissive
                };
                let n = v.len().max(1) as f32;
                let m = v
                    .iter()
                    .fold([0.0f32; 3], |a, x| [a[0] + x[0], a[1] + x[1], a[2] + x[2]]);
                let hex: String = m
                    .iter()
                    .map(|x| format!("{:02x}", (bake::to_srgb(x / n) * 255.0 + 0.5) as u8))
                    .collect();
                format!("mean #{hex}")
            }
            "normal" => {
                let flat = ch.normal.iter().map(|n| n[2]).fold(1.0f32, f32::min);
                format!("min z {flat:.2}")
            }
            "roughness" => fmt_span(span(&ch.roughness), ""),
            "metallic" => fmt_span(span(&ch.metallic), ""),
            "ao" => fmt_span(span(&ch.ao), ""),
            "opacity" => fmt_span(span(&ch.opacity), ""),
            "height" => {
                let (a, b) = span(&ch.height);
                format!("{} relief", crate::raster::fmt_len((b - a) as f64))
            }
            _ => String::new(),
        };
        out += &format!(" {c} {txt};");
    }
    if ch.driven.is_empty() {
        out += " none driven (defaults)";
    }
    out.trim_end_matches(';').to_string() + "\n"
}

fn fmt_span((a, b): (f32, f32), unit: &str) -> String {
    if (b - a).abs() < 1e-4 {
        format!("{a:.2}{unit}")
    } else {
        format!("{a:.2}..{b:.2}{unit}")
    }
}

/// A node's output as an image: numbers as grey (normalised to their range
/// when outside 0..1; SDFs show inside dark), vectors and colours as rgb,
/// ids as random colours. Returns the image and what the mapping was.
pub fn node_image(b: &Buf, w: usize, h: usize) -> (Canvas, String) {
    let n = w * h;
    let mut px = vec![255u8; n * 4];
    let u = |x: f32| (x.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    let note;
    match b {
        Buf::F(v) => {
            let (lo, hi) = v
                .iter()
                .fold((f32::MAX, f32::MIN), |(a, c), &x| (a.min(x), c.max(x)));
            let unit = lo >= -1e-4 && hi <= 1.0001;
            note = if unit {
                "0..1 as grey".to_string()
            } else {
                format!("{lo:.3}..{hi:.3} as black..white")
            };
            let (o, s) = if unit {
                (0.0, 1.0)
            } else {
                (lo, (hi - lo).max(1e-9))
            };
            px.par_chunks_mut(4).zip(v.par_iter()).for_each(|(p, x)| {
                let g = u((x - o) / s);
                p[..3].copy_from_slice(&[g, g, g]);
            });
        }
        Buf::V2(v) => {
            note = "x, y as red, green (fract)".to_string();
            px.par_chunks_mut(4).zip(v.par_iter()).for_each(|(p, x)| {
                p[..3].copy_from_slice(&[u(x[0] - x[0].floor()), u(x[1] - x[1].floor()), 0]);
            });
        }
        Buf::V3(v) => {
            note = "colour (sRGB)".to_string();
            px.par_chunks_mut(4).zip(v.par_iter()).for_each(|(p, x)| {
                p[..3].copy_from_slice(&x.map(|c| u(bake::to_srgb(c))));
            });
        }
        Buf::Id(v) => {
            note = "ids as random colours".to_string();
            px.par_chunks_mut(4).zip(v.par_iter()).for_each(|(p, x)| {
                let c = super::noise::hash3(*x as i32, 3, 1, 4);
                p[..3].copy_from_slice(&[(c >> 16) as u8, (c >> 8) as u8, c as u8]);
            });
        }
    }
    (Canvas::from_rgba(w, h, px), note)
}

/// A lit preview of a bake on a sphere (`cell` px), shaded on the CPU:
/// one key light, a sky/ground ambient and a GGX-ish highlight, with the
/// normal map applied in each texel's tangent frame.
pub fn sphere_preview(doc: &MatDoc, ch: &Channels, cell: usize, diameter: f32) -> Canvas {
    let mut c = Canvas::new(cell, cell, [24, 24, 28, 255]);
    let r = diameter * 0.5;
    let light = norm([-0.5, 0.8, 0.6]);
    let rows: Vec<Vec<[u8; 4]>> = (0..cell)
        .into_par_iter()
        .map(|py| {
            (0..cell)
                .map(|px| {
                    let x = (px as f32 + 0.5) / cell as f32 * 2.0 - 1.0;
                    let y = 1.0 - (py as f32 + 0.5) / cell as f32 * 2.0;
                    let d2 = x * x + y * y;
                    if d2 > 1.0 {
                        return [24, 24, 28, 255];
                    }
                    let nz = (1.0 - d2).sqrt();
                    let n = [x, y, nz];
                    // Longitude / colatitude in the sample grid's terms.
                    let th = n[1].clamp(-1.0, 1.0).acos();
                    let ph = n[0].atan2(n[2]).rem_euclid(std::f32::consts::TAU);
                    let i = if doc.domain == "object" {
                        let gx = ((ph / std::f32::consts::TAU) * ch.w as f32) as usize;
                        let gy = ((th / std::f32::consts::PI) * ch.h as f32) as usize;
                        gy.min(ch.h - 1) * ch.w + gx.min(ch.w - 1)
                    } else {
                        let (u, v) = (ph * r, th * r);
                        let t = doc.tile.max(1e-6);
                        let gx = ((u / t).rem_euclid(1.0) * ch.w as f32) as usize;
                        let gy = ((v / t).rem_euclid(1.0) * ch.h as f32) as usize;
                        gy.min(ch.h - 1) * ch.w + gx.min(ch.w - 1)
                    };
                    // Tangent frame: +u = east, image-up = north.
                    let east = norm([ph.cos(), 0.0, -ph.sin()]);
                    let north = norm(cross(n, east));
                    let tn = ch.normal[i];
                    let nw = norm([
                        east[0] * tn[0] + north[0] * tn[1] + n[0] * tn[2],
                        east[1] * tn[0] + north[1] * tn[1] + n[1] * tn[2],
                        east[2] * tn[0] + north[2] * tn[1] + n[2] * tn[2],
                    ]);
                    let col = shade(
                        ch.base_color[i],
                        ch.roughness[i],
                        ch.metallic[i],
                        ch.ao[i],
                        ch.emissive[i],
                        nw,
                        light,
                    );
                    let u8c = |v: f32| (bake::to_srgb(v) * 255.0 + 0.5) as u8;
                    [u8c(col[0]), u8c(col[1]), u8c(col[2]), 255]
                })
                .collect()
        })
        .collect();
    for (y, row) in rows.into_iter().enumerate() {
        for (x, p) in row.into_iter().enumerate() {
            c.set(x as i64, y as i64, p);
        }
    }
    c
}

fn norm(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-9);
    v.map(|c| c / l)
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn shade(
    base: [f32; 3],
    rough: f32,
    metal: f32,
    ao: f32,
    emis: [f32; 3],
    n: [f32; 3],
    l: [f32; 3],
) -> [f32; 3] {
    let v = [0.0, 0.0, 1.0];
    let h = norm([l[0] + v[0], l[1] + v[1], l[2] + v[2]]);
    let (nl, nh, nv) = (dot(n, l).max(0.0), dot(n, h).max(0.0), dot(n, v).max(1e-3));
    let a = (rough.clamp(0.04, 1.0)).powi(2);
    let a2 = a * a;
    let dd = nh * nh * (a2 - 1.0) + 1.0;
    let d = a2 / (std::f32::consts::PI * dd * dd);
    let k = (rough + 1.0).powi(2) / 8.0;
    let g = (nl / (nl * (1.0 - k) + k)) * (nv / (nv * (1.0 - k) + k));
    let f0 = [0, 1, 2].map(|c| 0.04 + (base[c] - 0.04) * metal);
    let vh = dot(v, h).max(0.0);
    let f = f0.map(|x| x + (1.0 - x) * (1.0 - vh).powi(5));
    let spec = f.map(|x| x * d * g / (4.0 * nl.max(1e-3) * nv));
    let sky = 0.25 + 0.15 * n[1];
    [0, 1, 2].map(|c| {
        let diff = base[c] * (1.0 - metal) / std::f32::consts::PI;
        let direct = (diff * (1.0 - f[c]) + spec[c]) * nl * 3.0;
        let amb = (base[c] * (1.0 - metal) + f0[c] * 0.5) * sky * ao;
        direct * (0.4 + 0.6 * ao) + amb + emis[c]
    })
}

/// The session's working materials: edited in place, each with its own
/// history tree, autosaved apart from the model.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Library {
    pub docs: BTreeMap<String, MatDoc>,
    /// The material last edited or viewed (the M pane shows it).
    #[serde(default)]
    pub active: Option<String>,
}

impl Library {
    /// Restores the autosave, if any.
    pub fn restore() -> Library {
        catalog::load_kind_state::<Library>(Kind::Materials).unwrap_or_default()
    }

    pub fn autosave(&self) {
        if let Err(e) = catalog::save_kind_state(Kind::Materials, self) {
            eprintln!("material autosave failed: {e}");
        }
    }

    /// Fingerprint for "did anything change" (versions and cursors).
    pub fn fingerprint(&self) -> Vec<(String, u64, Option<usize>, usize)> {
        self.docs
            .iter()
            .map(|(k, d)| {
                (
                    k.clone(),
                    d.version,
                    d.history.current,
                    d.history.nodes.len(),
                )
            })
            .chain(self.active.iter().map(|a| (a.clone(), u64::MAX, None, 0)))
            .collect()
    }

    pub fn get(&self, name: Option<&str>) -> Result<&MatDoc, String> {
        let name = match name {
            Some(n) => n.to_string(),
            None => self
                .active
                .clone()
                .ok_or("no material yet: create one with material_ops")?,
        };
        self.docs.get(&name).ok_or_else(|| {
            let names: Vec<&str> = self.docs.keys().map(String::as_str).collect();
            format!(
                "no working material `{name}` (working: {}; load a saved one with material_catalog load)",
                if names.is_empty() { "none".into() } else { names.join(", ") }
            )
        })
    }

    fn get_mut(&mut self, name: Option<&str>) -> Result<&mut MatDoc, String> {
        let name = self.get(name)?.name.clone();
        self.active = Some(name.clone());
        self.docs
            .get_mut(&name)
            .ok_or_else(|| format!("no material `{name}`"))
    }

    /// `material_ops`: edits (or creates) a working material.
    pub fn ops(
        &mut self,
        name: &str,
        items: Vec<Value>,
        params: Option<Params>,
        domain: Option<String>,
        tile: Option<f32>,
        resolution: Option<u32>,
        expect: Option<&MatExpect>,
    ) -> Result<(String, bool), String> {
        if name.is_empty() || name.contains('/') {
            return Err(format!("material: `{name}`: give a plain name"));
        }
        let mut doc = self
            .docs
            .get(name)
            .cloned()
            .unwrap_or_else(|| MatDoc::named(name));
        let fresh = !self.docs.contains_key(name);
        let r = doc.apply(items, params, domain, tile, resolution, expect)?;
        self.docs.insert(name.to_string(), doc);
        self.active = Some(name.to_string());
        Ok((
            if fresh {
                format!("new material `{name}`\n{}", r.0)
            } else {
                r.0
            },
            r.1,
        ))
    }

    pub fn undo(&mut self, name: Option<&str>) -> Result<String, String> {
        let d = self.get_mut(name)?;
        let (id, mut m) = d.history.undo().ok_or("nothing to undo")?;
        m.history = std::mem::take(&mut d.history);
        *d = m;
        Ok(format!(
            "material `{}` undone to #{id} (v{})",
            d.name, d.version
        ))
    }

    pub fn redo(&mut self, name: Option<&str>) -> Result<String, String> {
        let d = self.get_mut(name)?;
        let (id, mut m) = d.history.redo().ok_or("nothing to redo")?;
        m.history = std::mem::take(&mut d.history);
        *d = m;
        Ok(format!(
            "material `{}` redone to #{id} (v{})",
            d.name, d.version
        ))
    }

    pub fn jump(&mut self, name: Option<&str>, id: usize) -> Result<String, String> {
        let d = self.get_mut(name)?;
        let (_, mut m) = d.history.jump(id).map_err(|e| e.to_string())?;
        m.history = std::mem::take(&mut d.history);
        *d = m;
        Ok(format!(
            "material `{}` jumped to #{id} (v{}; editing here branches)",
            d.name, d.version
        ))
    }

    pub fn history(&self, name: Option<&str>) -> Result<String, String> {
        let d = self.get(name)?;
        Ok(format!(
            "material `{}` history:\n{}",
            d.name,
            d.history.describe()
        ))
    }

    /// `material_view`: a headless image of a material (lit sphere + channel
    /// thumbnails), or of one node's output.
    pub fn view(
        &mut self,
        name: Option<&str>,
        node: Option<&str>,
        channels: Option<Vec<String>>,
        size: Option<usize>,
        diameter: Option<f32>,
    ) -> Result<(String, Vec<u8>), String> {
        let doc = self.get(name)?.clone();
        self.active = Some(doc.name.clone());
        let cell = size.unwrap_or(256).clamp(64, 768);
        let n = (doc.resolution as usize).min(1024);
        if let Some(node) = node {
            let s = doc.samples(n, 0.25);
            let ev = eval::evaluate(&doc.nodes, &doc.params, &s, true)?;
            let (nm, port) = node.split_once('.').unwrap_or((node, ""));
            let ports = ev.ports.get(nm).ok_or_else(|| {
                let names: Vec<&str> = ev.nodes.iter().map(|p| p.name.as_str()).collect();
                format!("no node `{nm}` (nodes: {})", names.join(", "))
            })?;
            let (pname, b) = if port.is_empty() {
                ports
                    .first()
                    .map(|(p, b)| (*p, b.clone()))
                    .ok_or("that node has no outputs (output nodes: view the material)")?
            } else {
                ports
                    .iter()
                    .find(|(p, _)| *p == port)
                    .map(|(p, b)| (*p, b.clone()))
                    .ok_or_else(|| {
                        let names: Vec<&str> = ports.iter().map(|(p, _)| *p).collect();
                        format!("`{nm}` has no port `{port}` (ports: {})", names.join(", "))
                    })?
            };
            let (img, note) = node_image(&b, s.w, s.h);
            let scale = cell as f32 / s.w.max(s.h) as f32 * 2.0;
            let (ow, oh) = ((s.w as f32 * scale) as usize, (s.h as f32 * scale) as usize);
            let mut out = Canvas::new(ow.max(1), oh.max(1), [0, 0, 0, 255]);
            for y in 0..out.h {
                for x in 0..out.w {
                    let si = ((y * s.h / out.h) * s.w + x * s.w / out.w) * 4;
                    let di = (y * out.w + x) * 4;
                    out.px[di..di + 4].copy_from_slice(&img.px[si..si + 4]);
                }
            }
            let label = if pname.is_empty() {
                nm.to_string()
            } else {
                format!("{nm}.{pname}")
            };
            out.text(4, 4, &label, 2);
            let ports_list: Vec<&str> = ports
                .iter()
                .map(|(p, _)| *p)
                .filter(|p| !p.is_empty())
                .collect();
            return Ok((
                format!(
                    "node `{label}` ({}, {note}){}",
                    b.kind(),
                    if ports_list.is_empty() {
                        String::new()
                    } else {
                        format!("; ports: {}", ports_list.join(", "))
                    }
                ),
                out.to_png()?,
            ));
        }
        let ch = doc.bake(n, true)?;
        let names: Vec<String> = match channels {
            Some(c) => c,
            None => {
                let mut v: Vec<String> = vec!["base_color".into()];
                v.extend(
                    ch.driven
                        .iter()
                        .filter(|c| **c != "base_color")
                        .map(|c| c.to_string()),
                );
                v
            }
        };
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let dia = diameter.unwrap_or(if doc.domain == "tile" {
            doc.tile * 2.0
        } else {
            0.5
        });
        let sphere = sphere_preview(&doc, &ch, cell, dia);
        let thumbs = ch.grid(&refs, cell / 2, refs.len().div_ceil(2).max(1))?;
        let (w, h) = (sphere.w + thumbs.w + 4, sphere.h.max(thumbs.h));
        let mut out = Canvas::new(w, h, [24, 24, 28, 255]);
        for (img, x0) in [(&sphere, 0), (&thumbs, sphere.w + 4)] {
            for y in 0..img.h {
                let (s, d) = (y * img.w * 4, (y * w + x0) * 4);
                out.px[d..d + img.w * 4].copy_from_slice(&img.px[s..s + img.w * 4]);
            }
        }
        out.text(4, 4, &format!("{} ({:.2} m ball)", doc.name, dia), 1);
        Ok((
            format!(
                "material `{}` v{}: lit {:.2} m sphere + {}\n{}",
                doc.name,
                doc.version,
                dia,
                names.join(", "),
                channel_ranges(&ch)
            ),
            out.to_png()?,
        ))
    }

    /// Saves a working material to the library.
    pub fn save(
        &self,
        name: Option<&str>,
        description: &str,
        tags: &[&str],
        prompt: &str,
    ) -> Result<String, String> {
        let d = self.get(name)?;
        let (n, overwrote) =
            catalog::save_doc(Kind::Materials, &d.name, d, description, tags, prompt)
                .map_err(|e| e.to_string())?;
        Ok(if overwrote {
            format!("saved material `{n}` (overwrote the library entry)")
        } else {
            format!("saved material `{n}`")
        })
    }

    /// Loads a library material into the working set (replacing a working
    /// copy of the same name, which stays one material undo away).
    pub fn load(&mut self, name: &str) -> Result<String, String> {
        let mut doc: MatDoc =
            catalog::load_doc(Kind::Materials, name).map_err(|e| e.to_string())?;
        doc.check_format()?;
        if let Some(old) = self.docs.get(name) {
            doc.history = old.history.clone();
            doc.version = old.version + 1;
        }
        doc.history.record(doc.clone(), format!("load `{name}`"));
        let text = doc.describe();
        self.docs.insert(doc.name.clone(), doc);
        self.active = Some(name.to_string());
        Ok(format!("loaded material `{name}`\n{text}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn arr(v: Value) -> Vec<Value> {
        v.as_array().cloned().unwrap_or_default()
    }

    #[test]
    fn ops_upsert_roll_back_and_undo() {
        let mut lib = Library::default();
        let (t, _) = lib
            .ops(
                "stone",
                arr(json!([
                    {"type": "worley", "name": "cells", "size": 0.1},
                    {"type": "ramp", "name": "col", "input": "cells.edge", "stops": [[0, "#303030"], [0.03, "#8a8580"]]},
                    {"type": "output", "name": "out", "base_color": "col", "roughness": 0.8}
                ])),
                None,
                None,
                None,
                Some(64),
                None,
            )
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(t.contains("new material `stone`"), "{t}");
        // A bad item rolls back the whole batch.
        let e = lib
            .ops(
                "stone",
                arr(json!([
                    {"type": "output", "name": "out", "base_color": "col", "roughness": 0.3},
                    {"type": "noise", "name": "n", "size": 0.1, "bogus": 1}
                ])),
                None,
                None,
                None,
                None,
                None,
            )
            .err()
            .unwrap_or_default();
        assert!(e.contains("bogus"), "{e}");
        assert_eq!(lib.docs["stone"].version, 1);
        // A tweak, then undo back to the first version.
        lib.ops(
            "stone",
            arr(json!([{"type": "output", "name": "out", "base_color": "col", "roughness": 0.3}])),
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(lib.docs["stone"].version, 2);
        lib.undo(Some("stone")).unwrap_or_else(|e| panic!("{e}"));
        let d = &lib.docs["stone"];
        assert_eq!(d.version, 1);
        assert_eq!(d.nodes[2]["roughness"], json!(0.8));
        // Type change on an existing name is refused.
        let e = lib
            .ops(
                "stone",
                arr(json!([{"type": "noise", "name": "col", "size": 0.1}])),
                None,
                None,
                None,
                None,
                None,
            )
            .err()
            .unwrap_or_default();
        assert!(e.contains("is a ramp"), "{e}");
    }

    #[test]
    fn checks_catch_seams_and_dark_albedo() {
        let mut lib = Library::default();
        let exp = MatExpect {
            tileable: Some(true),
            ranges: Some(true),
            albedo: Some(Albedo::On(true)),
            normals: Some(true),
            coverage: None,
        };
        // A noise with a size that fits the tile: seamless.
        let (t, ok) = lib
            .ops(
                "good",
                arr(json!([
                    {"type": "noise", "name": "n", "size": 0.1, "octaves": 3},
                    {"type": "ramp", "name": "c", "input": "n", "stops": [[0, "#404040"], [1, "#b0b0b0"]]},
                    {"type": "math", "name": "h", "op": "mul", "a": "n", "b": 0.002},
                    {"type": "output", "name": "out", "base_color": "c", "height": "h"}
                ])),
                None,
                None,
                None,
                Some(64),
                Some(&exp),
            )
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(ok, "{t}");
        // A rotated stripe pattern does not wrap, and pure black is too dark.
        let (t, ok) = lib
            .ops(
                "bad",
                arr(json!([
                    {"type": "transform", "name": "rot", "rotate": 30},
                    {"type": "split", "name": "s", "input": "rot"},
                    {"type": "math", "name": "f", "op": "mul", "a": "s.x", "b": 7.3},
                    {"type": "math", "name": "st", "op": "fract", "a": "f"},
                    {"type": "output", "name": "out", "base_color": "st"}
                ])),
                None,
                None,
                None,
                Some(64),
                Some(&exp),
            )
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(!ok);
        assert!(t.contains("FAIL tileable"), "{t}");
        assert!(t.contains("FAIL albedo"), "{t}");
    }

    #[test]
    fn views_render_headless() {
        let mut lib = Library::default();
        lib.ops(
            "marble",
            arr(json!([
                {"type": "noise", "name": "n", "coords": "pos", "size": 0.08, "octaves": 5},
                {"type": "input", "name": "pos", "of": "position"},
                {"type": "split", "name": "p", "input": "pos"},
                {"type": "math", "name": "band", "op": "add", "a": "p.x", "b": "n"},
                {"type": "math", "name": "v", "op": "sin", "a": "=$k * 1"},
                {"type": "ramp", "name": "col", "input": "n", "stops": [[0.3, "#e8e4dc"], [0.7, "#6b6a70"]]},
                {"type": "output", "name": "out", "base_color": "col", "roughness": 0.2}
            ])),
            Some(Params::from([("k".to_string(), 3.0)])),
            Some("object".into()),
            None,
            Some(64),
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let (t, png) = lib
            .view(None, None, None, Some(128), None)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(t.contains("marble"), "{t}");
        assert!(png.len() > 1000);
        let (t, _) = lib
            .view(None, Some("n"), None, Some(128), None)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(t.contains("float"), "{t}");
        let e = lib
            .view(None, Some("p.w"), None, None, None)
            .err()
            .unwrap_or_default();
        assert!(e.contains("ports: x, y, z"), "{e}");
    }
}
