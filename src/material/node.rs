//! Material graph nodes: one strict serde struct per type (unknown fields are
//! rejected, like the modeler's op specs) and its kernel. Kernels map over the
//! sample set in parallel (rayon); every input is a [`Field`]: another node's
//! output buffer, or a constant broadcast to every sample.

use std::sync::Arc;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::eval::Samples;
use super::noise::{self, Lattice};
use super::sdf;

/// A node output: one value per sample.
#[derive(Debug, Clone)]
pub enum Buf {
    F(Vec<f32>),
    V2(Vec<[f32; 2]>),
    V3(Vec<[f32; 3]>),
    Id(Vec<u32>),
}

impl Buf {
    pub fn kind(&self) -> &'static str {
        match self {
            Buf::F(_) => "float",
            Buf::V2(_) => "vec2",
            Buf::V3(_) => "vec3/colour",
            Buf::Id(_) => "id",
        }
    }
}

/// What a node reads: a buffer, or a constant (a number or a vector/colour).
#[derive(Debug, Clone)]
pub enum Field {
    Buf(Arc<Buf>),
    F(f32),
    V(Vec<f32>),
}

impl Field {
    /// Components per sample: 1 (float, id), 2 or 3.
    pub fn dims(&self) -> usize {
        match self {
            Field::Buf(b) => match &**b {
                Buf::F(_) | Buf::Id(_) => 1,
                Buf::V2(_) => 2,
                Buf::V3(_) => 3,
            },
            Field::F(_) => 1,
            Field::V(v) => v.len().clamp(1, 3),
        }
    }

    /// As a number. A vector reads as the average of its components (a
    /// colour as its grey level); an id as a hash-random value in [0, 1).
    #[inline]
    pub fn f(&self, i: usize) -> f32 {
        match self {
            Field::F(x) => *x,
            Field::V(v) => v.iter().sum::<f32>() / v.len().max(1) as f32,
            Field::Buf(b) => match &**b {
                Buf::F(v) => v[i],
                Buf::V2(v) => (v[i][0] + v[i][1]) * 0.5,
                Buf::V3(v) => (v[i][0] + v[i][1] + v[i][2]) / 3.0,
                Buf::Id(v) => noise::unit(noise::hash3(v[i] as i32, 0, 0, 1)),
            },
        }
    }

    #[inline]
    pub fn v2(&self, i: usize) -> [f32; 2] {
        match self {
            Field::F(x) => [*x, *x],
            Field::V(v) => [v[0], *v.get(1).unwrap_or(&v[0])],
            Field::Buf(b) => match &**b {
                Buf::F(v) => [v[i], v[i]],
                Buf::V2(v) => v[i],
                Buf::V3(v) => [v[i][0], v[i][1]],
                Buf::Id(v) => {
                    let h = v[i] as i32;
                    [
                        noise::unit(noise::hash3(h, 1, 0, 2)),
                        noise::unit(noise::hash3(h, 2, 0, 2)),
                    ]
                }
            },
        }
    }

    #[inline]
    pub fn v3(&self, i: usize) -> [f32; 3] {
        match self {
            Field::F(x) => [*x; 3],
            Field::V(v) => {
                let g = |k: usize| *v.get(k).unwrap_or(&v[0]);
                if v.len() == 2 {
                    [v[0], v[1], 0.0]
                } else {
                    [g(0), g(1), g(2)]
                }
            }
            Field::Buf(b) => match &**b {
                Buf::F(v) => [v[i]; 3],
                Buf::V2(v) => [v[i][0], v[i][1], 0.0],
                Buf::V3(v) => v[i],
                Buf::Id(v) => {
                    let h = v[i] as i32;
                    [1, 2, 3].map(|k| noise::unit(noise::hash3(h, k, 0, 3)))
                }
            },
        }
    }

    /// As an integer id (ids pass through; anything else is hashed).
    #[inline]
    pub fn id(&self, i: usize) -> u32 {
        match self {
            Field::Buf(b) => match &**b {
                Buf::Id(v) => v[i],
                _ => {
                    let p = self.v3(i);
                    noise::hash3(
                        p[0].to_bits() as i32,
                        p[1].to_bits() as i32,
                        p[2].to_bits() as i32,
                        9,
                    )
                }
            },
            _ => {
                let p = self.v3(i);
                noise::hash3(
                    p[0].to_bits() as i32,
                    p[1].to_bits() as i32,
                    p[2].to_bits() as i32,
                    9,
                )
            }
        }
    }
}

/// An input: a number, a vector/colour `[..]`, `"#rrggbb"`, or another
/// node's output by name (`"bricks"` or `"bricks.id"`).
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum In {
    Num(f32),
    Vec(Vec<f32>),
    Name(String),
}

/// Parses `#rgb` / `#rrggbb` (sRGB, as colour pickers show them) into
/// linear 0..1 components.
pub fn hex(s: &str) -> Option<Vec<f32>> {
    hex_srgb(s).map(|c| c.into_iter().map(srgb_to_linear).collect())
}

/// sRGB transfer to linear light.
pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn hex_srgb(s: &str) -> Option<Vec<f32>> {
    let h = s.strip_prefix('#')?;
    let digits: Vec<u32> = h.chars().map(|c| c.to_digit(16)).collect::<Option<_>>()?;
    let c: Vec<f32> = match digits.len() {
        3 => digits.iter().map(|d| (*d * 17) as f32 / 255.0).collect(),
        6 | 8 => digits
            .chunks(2)
            .take(3)
            .map(|p| (p[0] * 16 + p[1]) as f32 / 255.0)
            .collect(),
        _ => return None,
    };
    Some(c)
}

/// The node outputs: (port, buffer), the first being the default port.
pub type Outs = Vec<(&'static str, Buf)>;

/// What a kernel sees: the samples and a resolver for its inputs.
pub struct Cx<'a> {
    pub s: &'a Samples,
    pub get: &'a (dyn Fn(&In) -> Result<Field, String> + Sync),
}

impl Cx<'_> {
    pub fn n(&self) -> usize {
        self.s.len()
    }
    pub fn input(&self, i: &In) -> Result<Field, String> {
        (self.get)(i)
    }
    pub fn or(&self, i: &Option<In>, default: Field) -> Result<Field, String> {
        match i {
            Some(i) => self.input(i),
            None => Ok(default),
        }
    }
    /// 2D coordinates (default: the sample's uv, meters).
    pub fn coords2(&self, i: &Option<In>) -> Result<Field, String> {
        self.or(i, Field::Buf(self.s.uv.clone()))
    }
    /// 3D coordinates (default: the sample's position, meters).
    pub fn coords3(&self, i: &Option<In>) -> Result<Field, String> {
        self.or(i, Field::Buf(self.s.pos.clone()))
    }
}

const MIN_LEN: usize = 2048;

pub fn pf(n: usize, f: impl Fn(usize) -> f32 + Sync + Send) -> Buf {
    Buf::F(
        (0..n)
            .into_par_iter()
            .with_min_len(MIN_LEN)
            .map(f)
            .collect(),
    )
}
pub fn pv2(n: usize, f: impl Fn(usize) -> [f32; 2] + Sync + Send) -> Buf {
    Buf::V2(
        (0..n)
            .into_par_iter()
            .with_min_len(MIN_LEN)
            .map(f)
            .collect(),
    )
}
pub fn pv3(n: usize, f: impl Fn(usize) -> [f32; 3] + Sync + Send) -> Buf {
    Buf::V3(
        (0..n)
            .into_par_iter()
            .with_min_len(MIN_LEN)
            .map(f)
            .collect(),
    )
}
pub fn pid(n: usize, f: impl Fn(usize) -> u32 + Sync + Send) -> Buf {
    Buf::Id(
        (0..n)
            .into_par_iter()
            .with_min_len(MIN_LEN)
            .map(f)
            .collect(),
    )
}

/// A kernel.
pub trait Eval {
    fn eval(&self, cx: &Cx) -> Result<Outs, String>;
}

macro_rules! node_types {
    ($( $ty:literal => $var:ident ),* $(,)?) => {
        /// One parsed node.
        #[derive(Clone, Debug)]
        pub enum Node { $($var($var)),* }

        /// Every node type, as written in `"type"`.
        pub const NODE_TYPES: &[&str] = &[$($ty),*];

        /// Parses a node body (without `type`/`name`) of type `ty`.
        pub fn parse_body(ty: &str, body: &Value, ctx: &str) -> Result<Node, String> {
            match ty {
                $($ty => Ok(Node::$var(de(ctx, body)?)),)*
                other => Err(format!(
                    "{ctx}: unknown node type `{other}` (one of: {})",
                    NODE_TYPES.join(", ")
                )),
            }
        }

        /// The JSON schema of a node type's fields.
        pub fn schema(ty: &str) -> Option<schemars::Schema> {
            match ty {
                $($ty => Some(schemars::schema_for!($var)),)*
                _ => None,
            }
        }

        impl Node {
            pub fn eval(&self, cx: &Cx) -> Result<Outs, String> {
                match self { $(Node::$var(n) => n.eval(cx)),* }
            }
        }
    };
}

fn de<T: serde::de::DeserializeOwned>(ctx: &str, body: &Value) -> Result<T, String> {
    serde_path_to_error::deserialize(body).map_err(|e| {
        let path = e.path().to_string();
        if path == "." {
            format!("{ctx}: {}", e.inner())
        } else {
            format!("{ctx}: {path}: {}", e.inner())
        }
    })
}

node_types! {
    "input" => Input,
    "transform" => Transform,
    "combine" => Combine,
    "split" => Split,
    "polar" => Polar,
    "kaleido" => Kaleido,
    "warp" => Warp,
    "tile_grid" => TileGrid,
    "repeat" => Repeat,
    "triplanar" => Triplanar,
    "circle" => Circle,
    "rect" => Rect,
    "segment" => Segment,
    "polygon" => Polygon,
    "star" => Star,
    "hexagon" => Hexagon,
    "arc" => Arc2,
    "rings" => Rings,
    "sphere" => Sphere,
    "box" => Box3,
    "torus" => Torus,
    "capsule" => Capsule,
    "cylinder" => Cylinder,
    "plane" => Plane,
    "sdf_op" => SdfOp,
    "sdf_round" => SdfRound,
    "onion" => Onion,
    "displace" => Displace,
    "mask" => Mask,
    "noise" => Noise,
    "worley" => Worley,
    "math" => Math,
    "remap" => Remap,
    "random" => Random,
    "color" => Color,
    "ramp" => Ramp,
    "hsv" => Hsv,
    "blend" => Blend,
    "levels" => Levels,
    "blur" => Blur,
    "normal_from_height" => NormalFromHeight,
    "ao_from_height" => AoFromHeight,
    "curvature" => Curvature,
    "dilate" => Dilate,
    "output" => Output,
}

// ---------------------------------------------------------------- coords

/// The sample's own coordinates: `uv` (meters, vec2), `position` (object
/// space meters, vec3) or `normal` (vec3).
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Input {
    /// "uv" (default), "position" or "normal".
    #[serde(default)]
    pub of: Option<String>,
}

impl Eval for Input {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let b = match self.of.as_deref().unwrap_or("uv") {
            "uv" => &cx.s.uv,
            "position" => &cx.s.pos,
            "normal" => &cx.s.nrm,
            o => return Err(format!("of: `{o}`: use uv, position or normal")),
        };
        Ok(vec![("", (**b).clone())])
    }
}

/// Scales, rotates (degrees, about the origin; about z for 3D) and offsets
/// coordinates, in that order.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Transform {
    /// Coordinates to transform (default uv). vec2 or vec3.
    #[serde(default)]
    pub coords: Option<In>,
    /// Multiplier: a number or per-axis vector (default 1).
    #[serde(default)]
    pub scale: Option<In>,
    /// Degrees counter-clockwise (default 0).
    #[serde(default)]
    pub rotate: Option<f32>,
    /// Added after scaling and rotating (default 0).
    #[serde(default)]
    pub offset: Option<In>,
}

impl Eval for Transform {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let c = cx.coords2(&self.coords)?;
        let s = cx.or(&self.scale, Field::F(1.0))?;
        let o = cx.or(&self.offset, Field::F(0.0))?;
        let (sn, cs) = self.rotate.unwrap_or(0.0).to_radians().sin_cos();
        let n = cx.n();
        Ok(vec![(
            "",
            if c.dims() == 3 {
                pv3(n, |i| {
                    let (p, k, d) = (c.v3(i), s.v3(i), o.v3(i));
                    let q = [p[0] * k[0], p[1] * k[1], p[2] * k[2]];
                    [
                        q[0] * cs - q[1] * sn + d[0],
                        q[0] * sn + q[1] * cs + d[1],
                        q[2] + d[2],
                    ]
                })
            } else {
                pv2(n, |i| {
                    let (p, k, d) = (c.v2(i), s.v2(i), o.v2(i));
                    let q = [p[0] * k[0], p[1] * k[1]];
                    [q[0] * cs - q[1] * sn + d[0], q[0] * sn + q[1] * cs + d[1]]
                })
            },
        )])
    }
}

/// Builds a vec2 (x, y) or vec3 (x, y, z) from numbers or fields.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Combine {
    pub x: In,
    pub y: In,
    #[serde(default)]
    pub z: Option<In>,
}

impl Eval for Combine {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let (x, y) = (cx.input(&self.x)?, cx.input(&self.y)?);
        let n = cx.n();
        Ok(vec![(
            "",
            match &self.z {
                Some(z) => {
                    let z = cx.input(z)?;
                    pv3(n, |i| [x.f(i), y.f(i), z.f(i)])
                }
                None => pv2(n, |i| [x.f(i), y.f(i)]),
            },
        )])
    }
}

/// Splits a vector into ports `x`, `y`, `z` (default port: x).
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Split {
    pub input: In,
}

impl Eval for Split {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let v = cx.input(&self.input)?;
        let n = cx.n();
        Ok(vec![
            ("x", pf(n, |i| v.v3(i)[0])),
            ("y", pf(n, |i| v.v3(i)[1])),
            ("z", pf(n, |i| v.v3(i)[2])),
        ])
    }
}

/// Polar coordinates around `center`: (angle in turns 0..1, radius meters).
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Polar {
    #[serde(default)]
    pub coords: Option<In>,
    #[serde(default)]
    pub center: Option<[f32; 2]>,
}

impl Eval for Polar {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let c = cx.coords2(&self.coords)?;
        let o = self.center.unwrap_or([0.0; 2]);
        Ok(vec![(
            "",
            pv2(cx.n(), |i| {
                let p = c.v2(i);
                let (x, y) = (p[0] - o[0], p[1] - o[1]);
                [
                    y.atan2(x) / std::f32::consts::TAU + 0.5,
                    (x * x + y * y).sqrt(),
                ]
            }),
        )])
    }
}

/// Folds coordinates into `segments` mirrored wedges around `center`.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Kaleido {
    #[serde(default)]
    pub coords: Option<In>,
    pub segments: u32,
    #[serde(default)]
    pub center: Option<[f32; 2]>,
}

impl Eval for Kaleido {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let c = cx.coords2(&self.coords)?;
        let o = self.center.unwrap_or([0.0; 2]);
        let w = std::f32::consts::TAU / self.segments.max(1) as f32;
        Ok(vec![(
            "",
            pv2(cx.n(), |i| {
                let p = c.v2(i);
                let (x, y) = (p[0] - o[0], p[1] - o[1]);
                let r = (x * x + y * y).sqrt();
                let a = y.atan2(x).rem_euclid(w);
                let a = if a > w * 0.5 { w - a } else { a };
                [o[0] + r * a.cos(), o[1] + r * a.sin()]
            }),
        )])
    }
}

/// Domain warp: coords + (by − 0.5) × amount (`by` is typically a 0..1
/// noise; set `centered: false` to add `by` as is).
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Warp {
    #[serde(default)]
    pub coords: Option<In>,
    /// The displacement field (a number field shifts every axis alike).
    pub by: In,
    /// Meters of displacement at by = 1 (default 0.1).
    #[serde(default)]
    pub amount: Option<In>,
    #[serde(default)]
    pub centered: Option<bool>,
}

impl Eval for Warp {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let c = cx.coords2(&self.coords)?;
        let by = cx.input(&self.by)?;
        let amt = cx.or(&self.amount, Field::F(0.1))?;
        let k = if self.centered.unwrap_or(true) {
            0.5
        } else {
            0.0
        };
        let n = cx.n();
        // A scalar field warps each axis with a decorrelated copy of itself
        // (read at a shifted index would break locality), so use it for x
        // and its 1 - value for y: cheap and still non-uniform.
        let scalar = by.dims() == 1;
        Ok(vec![(
            "",
            if c.dims() == 3 {
                pv3(n, |i| {
                    let (p, a) = (c.v3(i), amt.f(i));
                    let d = if scalar {
                        let f = by.f(i);
                        [f, 1.0 - f, f]
                    } else {
                        by.v3(i)
                    };
                    [
                        p[0] + (d[0] - k) * a,
                        p[1] + (d[1] - k) * a,
                        p[2] + (d[2] - k) * a,
                    ]
                })
            } else {
                pv2(n, |i| {
                    let (p, a) = (c.v2(i), amt.f(i));
                    let d = if scalar {
                        let f = by.f(i);
                        [f, 1.0 - f]
                    } else {
                        by.v2(i)
                    };
                    [p[0] + (d[0] - k) * a, p[1] + (d[1] - k) * a]
                })
            },
        )])
    }
}

/// A grid of `size` [w, h] meter cells (bricks, tiles, planks); every other
/// row shifted by `stagger` × w. Ports: `local` (meters from the cell centre,
/// default), `cell` (0..1 within the cell), `id` (per-cell id), `edge`
/// (meters to the nearest cell border: mortar = edge < gap / 2).
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TileGrid {
    #[serde(default)]
    pub coords: Option<In>,
    pub size: [f32; 2],
    /// Row offset as a fraction of the cell width (0.5 = running bond).
    #[serde(default)]
    pub stagger: Option<f32>,
}

impl Eval for TileGrid {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let c = cx.coords2(&self.coords)?;
        let [w, h] = [self.size[0].max(1e-6), self.size[1].max(1e-6)];
        let st = self.stagger.unwrap_or(0.0);
        let n = cx.n();
        // Cells along each tile edge, for wrapping ids at the tile period.
        let per = |s: f32| {
            if cx.s.period > 0.0 {
                (cx.s.period / s).round().max(1.0) as i32
            } else {
                0
            }
        };
        let (px, py) = (per(w), per(h));
        let cell = |i: usize| {
            let p = c.v2(i);
            let row = (p[1] / h).floor();
            let x = p[0] - st * w * row.rem_euclid(2.0);
            let col = (x / w).floor();
            let lx = x - (col + 0.5) * w;
            let ly = p[1] - (row + 0.5) * h;
            (col as i32, row as i32, lx, ly)
        };
        let wrap = |v: i32, p: i32| if p > 0 { v.rem_euclid(p) } else { v };
        Ok(vec![
            (
                "local",
                pv2(n, |i| {
                    let (_, _, x, y) = cell(i);
                    [x, y]
                }),
            ),
            (
                "cell",
                pv2(n, |i| {
                    let (_, _, x, y) = cell(i);
                    [x / w + 0.5, y / h + 0.5]
                }),
            ),
            (
                "id",
                pid(n, |i| {
                    let (cx_, cy, _, _) = cell(i);
                    noise::hash3(wrap(cx_, px), wrap(cy, py), 0, 77)
                }),
            ),
            (
                "edge",
                pf(n, |i| {
                    let (_, _, x, y) = cell(i);
                    (w * 0.5 - x.abs()).min(h * 0.5 - y.abs())
                }),
            ),
        ])
    }
}

/// Domain repetition: coordinates folded into cells of `period` meters
/// (2 or 3 values), centred. Ports: `local` (default) and `id`.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Repeat {
    #[serde(default)]
    pub coords: Option<In>,
    pub period: Vec<f32>,
}

impl Eval for Repeat {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        if self.period.is_empty() {
            return Err("period: give 2 or 3 lengths in meters".into());
        }
        let p3 = cx.s.uv.clone();
        let c = match &self.coords {
            Some(i) => cx.input(i)?,
            None => Field::Buf(p3),
        };
        let per: Vec<f32> = (0..3)
            .map(|k| self.period.get(k).copied().unwrap_or(0.0))
            .collect();
        let n = cx.n();
        let fold = move |v: f32, p: f32| {
            if p > 0.0 {
                let k = (v / p).floor();
                (v - (k + 0.5) * p, k as i32)
            } else {
                (v, 0)
            }
        };
        let three = c.dims() == 3;
        let local = if three {
            pv3(n, |i| {
                let q = c.v3(i);
                [
                    fold(q[0], per[0]).0,
                    fold(q[1], per[1]).0,
                    fold(q[2], per[2]).0,
                ]
            })
        } else {
            pv2(n, |i| {
                let q = c.v2(i);
                [fold(q[0], per[0]).0, fold(q[1], per[1]).0]
            })
        };
        let id = pid(n, |i| {
            let q = c.v3(i);
            noise::hash3(
                fold(q[0], per[0]).1,
                fold(q[1], per[1]).1,
                fold(q[2], per[2]).1,
                78,
            )
        });
        Ok(vec![("local", local), ("id", id)])
    }
}

/// Box (triplanar) projection of 3D positions to 2D: each sample uses the
/// plane facing its normal's dominant axis, so a 2D pattern wraps an object
/// without stretching. Default coords: position; default normal: the
/// sample's.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Triplanar {
    #[serde(default)]
    pub coords: Option<In>,
}

impl Eval for Triplanar {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let c = cx.coords3(&self.coords)?;
        let nrm = Field::Buf(cx.s.nrm.clone());
        Ok(vec![(
            "",
            pv2(cx.n(), |i| {
                let (p, n) = (c.v3(i), nrm.v3(i));
                let a = [n[0].abs(), n[1].abs(), n[2].abs()];
                if a[0] >= a[1] && a[0] >= a[2] {
                    [-n[0].signum() * p[2], p[1]]
                } else if a[1] >= a[2] {
                    [p[0], -n[1].signum() * p[2]]
                } else {
                    [n[2].signum() * p[0], p[1]]
                }
            }),
        )])
    }
}

// ---------------------------------------------------------------- 2D SDFs

fn at2(p: [f32; 2], c: Option<[f32; 2]>) -> [f32; 2] {
    let c = c.unwrap_or([0.0; 2]);
    [p[0] - c[0], p[1] - c[1]]
}

fn at3(p: [f32; 3], c: Option<[f32; 3]>) -> [f32; 3] {
    let c = c.unwrap_or([0.0; 3]);
    [p[0] - c[0], p[1] - c[1], p[2] - c[2]]
}

/// Circle SDF (negative inside), radius may be a field.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Circle {
    #[serde(default)]
    pub coords: Option<In>,
    pub radius: In,
    #[serde(default)]
    pub center: Option<[f32; 2]>,
}

impl Eval for Circle {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let (c, r) = (cx.coords2(&self.coords)?, cx.input(&self.radius)?);
        Ok(vec![(
            "",
            pf(cx.n(), |i| sdf::circle(at2(c.v2(i), self.center), r.f(i))),
        )])
    }
}

/// Rectangle SDF: full `size` [w, h], corners rounded by `round`.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Rect {
    #[serde(default)]
    pub coords: Option<In>,
    pub size: In,
    #[serde(default)]
    pub center: Option<[f32; 2]>,
    #[serde(default)]
    pub round: Option<In>,
}

impl Eval for Rect {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let (c, s) = (cx.coords2(&self.coords)?, cx.input(&self.size)?);
        let r = cx.or(&self.round, Field::F(0.0))?;
        Ok(vec![(
            "",
            pf(cx.n(), |i| {
                let z = s.v2(i);
                sdf::rect(at2(c.v2(i), self.center), [z[0] * 0.5, z[1] * 0.5], r.f(i))
            }),
        )])
    }
}

/// Segment from `a` to `b`, `width` wide (a line, a crack, a seam).
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Segment {
    #[serde(default)]
    pub coords: Option<In>,
    pub a: [f32; 2],
    pub b: [f32; 2],
    pub width: In,
}

impl Eval for Segment {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let (c, w) = (cx.coords2(&self.coords)?, cx.input(&self.width)?);
        Ok(vec![(
            "",
            pf(cx.n(), |i| sdf::segment(c.v2(i), self.a, self.b, w.f(i))),
        )])
    }
}

/// Closed polygon SDF through `points`.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Polygon {
    #[serde(default)]
    pub coords: Option<In>,
    pub points: Vec<[f32; 2]>,
}

impl Eval for Polygon {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        if self.points.len() < 3 {
            return Err("points: a polygon needs at least 3".into());
        }
        let c = cx.coords2(&self.coords)?;
        Ok(vec![(
            "",
            pf(cx.n(), |i| sdf::polygon(c.v2(i), &self.points)),
        )])
    }
}

/// Star SDF: `points` tips at `radius`, notches at `inner`.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Star {
    #[serde(default)]
    pub coords: Option<In>,
    pub radius: f32,
    pub inner: f32,
    pub points: u32,
    #[serde(default)]
    pub center: Option<[f32; 2]>,
}

impl Eval for Star {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let c = cx.coords2(&self.coords)?;
        Ok(vec![(
            "",
            pf(cx.n(), |i| {
                sdf::star(
                    at2(c.v2(i), self.center),
                    self.radius,
                    self.inner,
                    self.points,
                )
            }),
        )])
    }
}

/// Regular hexagon SDF (apothem `radius`).
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Hexagon {
    #[serde(default)]
    pub coords: Option<In>,
    pub radius: In,
    #[serde(default)]
    pub center: Option<[f32; 2]>,
}

impl Eval for Hexagon {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let (c, r) = (cx.coords2(&self.coords)?, cx.input(&self.radius)?);
        Ok(vec![(
            "",
            pf(cx.n(), |i| sdf::hexagon(at2(c.v2(i), self.center), r.f(i))),
        )])
    }
}

/// Arc stroke: radius, from/to degrees, width.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Arc2 {
    #[serde(default)]
    pub coords: Option<In>,
    pub radius: f32,
    pub from: f32,
    pub to: f32,
    pub width: f32,
    #[serde(default)]
    pub center: Option<[f32; 2]>,
}

impl Eval for Arc2 {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let c = cx.coords2(&self.coords)?;
        Ok(vec![(
            "",
            pf(cx.n(), |i| {
                sdf::arc(
                    at2(c.v2(i), self.center),
                    self.radius,
                    self.from,
                    self.to,
                    self.width,
                )
            }),
        )])
    }
}

/// Distance to concentric rings `spacing` apart (wood grain, ripples).
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Rings {
    #[serde(default)]
    pub coords: Option<In>,
    pub spacing: f32,
    #[serde(default)]
    pub center: Option<[f32; 2]>,
}

impl Eval for Rings {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let c = cx.coords2(&self.coords)?;
        Ok(vec![(
            "",
            pf(cx.n(), |i| {
                sdf::rings(at2(c.v2(i), self.center), self.spacing)
            }),
        )])
    }
}

// ---------------------------------------------------------------- 3D SDFs

/// Sphere SDF on 3D coordinates (default: the sample's position).
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Sphere {
    #[serde(default)]
    pub coords: Option<In>,
    pub radius: In,
    #[serde(default)]
    pub center: Option<[f32; 3]>,
}

impl Eval for Sphere {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let (c, r) = (cx.coords3(&self.coords)?, cx.input(&self.radius)?);
        Ok(vec![(
            "",
            pf(cx.n(), |i| sdf::sphere(at3(c.v3(i), self.center), r.f(i))),
        )])
    }
}

/// Box SDF: full `size` [x, y, z], edges rounded by `round`.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Box3 {
    #[serde(default)]
    pub coords: Option<In>,
    pub size: In,
    #[serde(default)]
    pub center: Option<[f32; 3]>,
    #[serde(default)]
    pub round: Option<In>,
}

impl Eval for Box3 {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let (c, s) = (cx.coords3(&self.coords)?, cx.input(&self.size)?);
        let r = cx.or(&self.round, Field::F(0.0))?;
        Ok(vec![(
            "",
            pf(cx.n(), |i| {
                let z = s.v3(i);
                sdf::boxd(at3(c.v3(i), self.center), z.map(|v| v * 0.5), r.f(i))
            }),
        )])
    }
}

/// Torus around y: `major` ring radius, `minor` tube radius.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Torus {
    #[serde(default)]
    pub coords: Option<In>,
    pub major: f32,
    pub minor: f32,
    #[serde(default)]
    pub center: Option<[f32; 3]>,
}

impl Eval for Torus {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let c = cx.coords3(&self.coords)?;
        Ok(vec![(
            "",
            pf(cx.n(), |i| {
                sdf::torus(at3(c.v3(i), self.center), self.major, self.minor)
            }),
        )])
    }
}

/// Capsule from `a` to `b` with `radius`.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Capsule {
    #[serde(default)]
    pub coords: Option<In>,
    pub a: [f32; 3],
    pub b: [f32; 3],
    pub radius: In,
}

impl Eval for Capsule {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let (c, r) = (cx.coords3(&self.coords)?, cx.input(&self.radius)?);
        Ok(vec![(
            "",
            pf(cx.n(), |i| sdf::capsule(c.v3(i), self.a, self.b, r.f(i))),
        )])
    }
}

/// Capped cylinder along y: `radius`, full `height`.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Cylinder {
    #[serde(default)]
    pub coords: Option<In>,
    pub radius: f32,
    pub height: f32,
    #[serde(default)]
    pub center: Option<[f32; 3]>,
}

impl Eval for Cylinder {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let c = cx.coords3(&self.coords)?;
        Ok(vec![(
            "",
            pf(cx.n(), |i| {
                sdf::cylinder(at3(c.v3(i), self.center), self.radius, self.height * 0.5)
            }),
        )])
    }
}

/// Plane SDF: positive on `normal`'s side, `offset` meters from the origin.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Plane {
    #[serde(default)]
    pub coords: Option<In>,
    pub normal: [f32; 3],
    #[serde(default)]
    pub offset: Option<f32>,
}

impl Eval for Plane {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let c = cx.coords3(&self.coords)?;
        let o = self.offset.unwrap_or(0.0);
        Ok(vec![(
            "",
            pf(cx.n(), |i| sdf::plane(c.v3(i), self.normal, o)),
        )])
    }
}

// ---------------------------------------------------------------- SDF ops

/// Combines two SDFs: union, intersect, subtract (a minus b), or their
/// smooth_ versions with blend radius `k` meters.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SdfOp {
    pub op: String,
    pub a: In,
    pub b: In,
    #[serde(default)]
    pub k: Option<f32>,
}

impl Eval for SdfOp {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let (a, b) = (cx.input(&self.a)?, cx.input(&self.b)?);
        let k = self.k.unwrap_or(0.0);
        let op = self.op.as_str();
        if !matches!(
            op,
            "union"
                | "intersect"
                | "subtract"
                | "smooth_union"
                | "smooth_intersect"
                | "smooth_subtract"
        ) {
            return Err(format!(
                "op: `{op}`: use union, intersect, subtract or smooth_union / smooth_intersect / smooth_subtract"
            ));
        }
        let smooth = op.starts_with("smooth_");
        let kk = if smooth { k.max(1e-6) } else { 0.0 };
        Ok(vec![(
            "",
            pf(cx.n(), |i| {
                let (x, y) = (a.f(i), b.f(i));
                match op.trim_start_matches("smooth_") {
                    "union" => sdf::smin(x, y, kk),
                    "intersect" => -sdf::smin(-x, -y, kk),
                    _ => -sdf::smin(-x, y, kk),
                }
            }),
        )])
    }
}

/// Rounds an SDF: d − radius (grows and rounds corners).
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SdfRound {
    pub input: In,
    pub radius: In,
}

impl Eval for SdfRound {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let (d, r) = (cx.input(&self.input)?, cx.input(&self.radius)?);
        Ok(vec![("", pf(cx.n(), |i| d.f(i) - r.f(i)))])
    }
}

/// Hollows an SDF into a shell `thickness` wide: |d| − thickness/2.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Onion {
    pub input: In,
    pub thickness: In,
}

impl Eval for Onion {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let (d, t) = (cx.input(&self.input)?, cx.input(&self.thickness)?);
        Ok(vec![("", pf(cx.n(), |i| d.f(i).abs() - t.f(i) * 0.5))])
    }
}

/// Adds (by − 0.5) × amount to an SDF: noisy, organic edges.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Displace {
    pub input: In,
    pub by: In,
    pub amount: In,
}

impl Eval for Displace {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let (d, b, a) = (
            cx.input(&self.input)?,
            cx.input(&self.by)?,
            cx.input(&self.amount)?,
        );
        Ok(vec![("", pf(cx.n(), |i| d.f(i) + (b.f(i) - 0.5) * a.f(i)))])
    }
}

/// Turns an SDF into a 0..1 mask, anti-aliased over a sample: `fill`
/// (inside = 1), `stroke` (a line `width` wide on the edge) or `glow`
/// (falls off over `width` outside). `softness` widens the edge (meters).
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Mask {
    pub sdf: In,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub width: Option<f32>,
    #[serde(default)]
    pub softness: Option<f32>,
}

impl Eval for Mask {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let d = cx.input(&self.sdf)?;
        let soft = self.softness.unwrap_or(0.0).max(cx.s.texel);
        let w = self.width.unwrap_or(cx.s.texel * 2.0);
        let mode = self.mode.as_deref().unwrap_or("fill");
        if !matches!(mode, "fill" | "stroke" | "glow") {
            return Err(format!("mode: `{mode}`: use fill, stroke or glow"));
        }
        let ss = |e0: f32, e1: f32, x: f32| {
            let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        };
        Ok(vec![(
            "",
            pf(cx.n(), |i| {
                let v = d.f(i);
                match mode {
                    "fill" => 1.0 - ss(-soft * 0.5, soft * 0.5, v),
                    "stroke" => 1.0 - ss(w * 0.5 - soft * 0.5, w * 0.5 + soft * 0.5, v.abs()),
                    _ => 1.0 - ss(0.0, w.max(1e-6), v.max(0.0)),
                }
            }),
        )])
    }
}

// ---------------------------------------------------------------- noise

/// Lattice noise in 0..1 on 2D or 3D coordinates: `kind` value or
/// gradient, feature `size` meters, `octaves` of fbm / ridged / turbulence.
/// In the tile domain the lattice snaps to a whole number of cells per tile,
/// so the texture repeats with no seam.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Noise {
    #[serde(default)]
    pub coords: Option<In>,
    #[serde(default)]
    pub kind: Option<String>,
    pub size: f32,
    #[serde(default)]
    pub seed: Option<u32>,
    #[serde(default)]
    pub octaves: Option<u32>,
    #[serde(default)]
    pub lacunarity: Option<f32>,
    #[serde(default)]
    pub gain: Option<f32>,
    /// "fbm" (default), "ridged" or "turbulence".
    #[serde(default)]
    pub mode: Option<String>,
}

impl Eval for Noise {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let c = cx.coords2(&self.coords)?;
        let dims = c.dims().max(2);
        let l = Lattice::new(self.size, cx.s.period);
        let kind = self.kind.as_deref().unwrap_or("gradient");
        if !matches!(kind, "value" | "gradient") {
            return Err(format!("kind: `{kind}`: use value or gradient"));
        }
        let mode = self.mode.as_deref().unwrap_or("fbm");
        if !matches!(mode, "fbm" | "ridged" | "turbulence") {
            return Err(format!("mode: `{mode}`: use fbm, ridged or turbulence"));
        }
        let (seed, oct) = (
            self.seed.unwrap_or(0),
            self.octaves.unwrap_or(1).clamp(1, 12),
        );
        let (lac, gain) = (self.lacunarity.unwrap_or(2.0), self.gain.unwrap_or(0.5));
        Ok(vec![(
            "",
            pf(cx.n(), |i| {
                let p = c.v3(i);
                let base = |p: [f32; 3], l: Lattice, s: u32| {
                    if kind == "value" {
                        noise::value(p, dims, l, s)
                    } else {
                        noise::gradient(p, dims, l, s)
                    }
                };
                noise::fractal(base, p, l, seed, oct, lac, gain, mode)
            }),
        )])
    }
}

/// Worley / cellular noise with feature `size` meters. Ports: `f1`
/// (default; meters to the nearest point), `f2`, `edge` (f2 − f1: cell
/// borders, cracks, stones) and `id` (per cell).
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Worley {
    #[serde(default)]
    pub coords: Option<In>,
    pub size: f32,
    #[serde(default)]
    pub seed: Option<u32>,
    /// 0 = a regular grid, 1 (default) = fully random points.
    #[serde(default)]
    pub jitter: Option<f32>,
}

impl Eval for Worley {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let c = cx.coords2(&self.coords)?;
        let dims = c.dims().max(2);
        let l = Lattice::new(self.size, cx.s.period);
        let (seed, jit) = (self.seed.unwrap_or(0), self.jitter.unwrap_or(1.0));
        let n = cx.n();
        let cells: Vec<noise::Cell> = (0..n)
            .into_par_iter()
            .with_min_len(MIN_LEN)
            .map(|i| noise::worley(c.v3(i), dims, l, seed, jit))
            .collect();
        Ok(vec![
            ("f1", Buf::F(cells.iter().map(|c| c.f1).collect())),
            ("f2", Buf::F(cells.iter().map(|c| c.f2).collect())),
            ("edge", Buf::F(cells.iter().map(|c| c.f2 - c.f1).collect())),
            ("id", Buf::Id(cells.iter().map(|c| c.id).collect())),
        ])
    }
}

// ---------------------------------------------------------------- math

/// Arithmetic on numbers, vectors or colours (per component, broadcasting
/// numbers). Binary: add, sub, mul, div, min, max, pow. mix (a→b by c),
/// clamp (a into [b, c]), step (0/1: a ≥ b), smoothstep (a over b..c).
/// Unary: abs, invert (1 − a), sin, fract, sqrt, length (vector → number),
/// negate.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Math {
    pub op: String,
    pub a: In,
    #[serde(default)]
    pub b: Option<In>,
    #[serde(default)]
    pub c: Option<In>,
}

const MATH_OPS: &[&str] = &[
    "add",
    "sub",
    "mul",
    "div",
    "min",
    "max",
    "pow",
    "mix",
    "clamp",
    "step",
    "smoothstep",
    "abs",
    "invert",
    "sin",
    "fract",
    "sqrt",
    "length",
    "negate",
];

impl Eval for Math {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let op = self.op.as_str();
        if !MATH_OPS.contains(&op) {
            return Err(format!("op: `{op}`: use one of {}", MATH_OPS.join(", ")));
        }
        let a = cx.input(&self.a)?;
        let needs_b = !matches!(
            op,
            "abs" | "invert" | "sin" | "fract" | "sqrt" | "length" | "negate"
        );
        if needs_b && self.b.is_none() {
            return Err(format!("b: `{op}` needs b"));
        }
        if matches!(op, "mix" | "clamp" | "smoothstep") && self.c.is_none() {
            return Err(format!("c: `{op}` needs c"));
        }
        let b = cx.or(&self.b, Field::F(0.0))?;
        let c = cx.or(&self.c, Field::F(0.0))?;
        let f = |x: f32, y: f32, z: f32| -> f32 {
            match op {
                "add" => x + y,
                "sub" => x - y,
                "mul" => x * y,
                "div" => {
                    if y.abs() < 1e-12 {
                        0.0
                    } else {
                        x / y
                    }
                }
                "min" => x.min(y),
                "max" => x.max(y),
                "pow" => x.max(0.0).powf(y),
                "mix" => x + (y - x) * z,
                "clamp" => x.clamp(y.min(z), y.max(z)),
                "step" => f32::from(u8::from(x >= y)),
                "smoothstep" => {
                    let t = ((x - y) / (z - y).max(1e-12)).clamp(0.0, 1.0);
                    t * t * (3.0 - 2.0 * t)
                }
                "abs" => x.abs(),
                "invert" => 1.0 - x,
                "sin" => x.sin(),
                "fract" => x - x.floor(),
                "sqrt" => x.max(0.0).sqrt(),
                "negate" => -x,
                _ => x,
            }
        };
        let n = cx.n();
        if op == "length" {
            return Ok(vec![(
                "",
                pf(n, |i| {
                    let v = a.v3(i);
                    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
                }),
            )]);
        }
        let dims = a.dims().max(if needs_b { b.dims() } else { 1 });
        Ok(vec![(
            "",
            match dims {
                1 => pf(n, |i| f(a.f(i), b.f(i), c.f(i))),
                2 => pv2(n, |i| {
                    let (x, y, z) = (a.v2(i), b.v2(i), c.v2(i));
                    [f(x[0], y[0], z[0]), f(x[1], y[1], z[1])]
                }),
                _ => pv3(n, |i| {
                    let (x, y, z) = (a.v3(i), b.v3(i), c.v3(i));
                    [
                        f(x[0], y[0], z[0]),
                        f(x[1], y[1], z[1]),
                        f(x[2], y[2], z[2]),
                    ]
                }),
            },
        )])
    }
}

/// Maps a number from `from` [lo, hi] to `to` [lo, hi] (clamped unless
/// `clamp: false`).
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Remap {
    pub input: In,
    pub from: [f32; 2],
    pub to: [f32; 2],
    #[serde(default)]
    pub clamp: Option<bool>,
}

impl Eval for Remap {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let a = cx.input(&self.input)?;
        let (f, t) = (self.from, self.to);
        let cl = self.clamp.unwrap_or(true);
        Ok(vec![(
            "",
            pf(cx.n(), |i| {
                let mut u = (a.f(i) - f[0]) / (f[1] - f[0]).max(1e-12);
                if cl {
                    u = u.clamp(0.0, 1.0);
                }
                t[0] + (t[1] - t[0]) * u
            }),
        )])
    }
}

/// A random value per id (cell, brick, plank): `out` float (default, in
/// [min, max]) or color (random rgb in [min, max]).
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Random {
    pub id: In,
    #[serde(default)]
    pub seed: Option<u32>,
    #[serde(default)]
    pub out: Option<String>,
    #[serde(default)]
    pub min: Option<f32>,
    #[serde(default)]
    pub max: Option<f32>,
}

impl Eval for Random {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let id = cx.input(&self.id)?;
        let s = self.seed.unwrap_or(0);
        let (lo, hi) = (self.min.unwrap_or(0.0), self.max.unwrap_or(1.0));
        let r =
            |i: usize, k: i32| lo + (hi - lo) * noise::unit(noise::hash3(id.id(i) as i32, k, 5, s));
        Ok(vec![(
            "",
            match self.out.as_deref().unwrap_or("float") {
                "float" => pf(cx.n(), |i| r(i, 0)),
                "color" => pv3(cx.n(), |i| [r(i, 1), r(i, 2), r(i, 3)]),
                o => return Err(format!("out: `{o}`: use float or color")),
            },
        )])
    }
}

// ---------------------------------------------------------------- colour

/// A constant colour or vector.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Color {
    pub value: In,
}

impl Eval for Color {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let v = cx.input(&self.value)?;
        Ok(vec![("", pv3(cx.n(), |i| v.v3(i)))])
    }
}

/// Maps a number through colour stops `[[t, colour], ...]` (t ascending).
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Ramp {
    pub input: In,
    pub stops: Vec<(f32, In)>,
}

impl Eval for Ramp {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        if self.stops.is_empty() {
            return Err("stops: give at least one [t, colour]".into());
        }
        let a = cx.input(&self.input)?;
        let mut stops: Vec<(f32, [f32; 3])> = Vec::new();
        for (k, (t, c)) in self.stops.iter().enumerate() {
            let f = cx.input(c).map_err(|e| format!("stops[{k}]: {e}"))?;
            if matches!(f, Field::Buf(_)) {
                return Err(format!("stops[{k}]: stop colours must be constants"));
            }
            stops.push((*t, f.v3(0)));
        }
        stops.sort_by(|x, y| x.0.total_cmp(&y.0));
        Ok(vec![(
            "",
            pv3(cx.n(), |i| {
                let x = a.f(i);
                let k = stops.partition_point(|s| s.0 <= x);
                if k == 0 {
                    return stops[0].1;
                }
                if k >= stops.len() {
                    return stops[stops.len() - 1].1;
                }
                let ((t0, c0), (t1, c1)) = (stops[k - 1], stops[k]);
                let u = ((x - t0) / (t1 - t0).max(1e-12)).clamp(0.0, 1.0);
                [
                    c0[0] + (c1[0] - c0[0]) * u,
                    c0[1] + (c1[1] - c0[1]) * u,
                    c0[2] + (c1[2] - c0[2]) * u,
                ]
            }),
        )])
    }
}

/// Hue shift (turns), saturation and value multipliers; each may be a field.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Hsv {
    pub input: In,
    #[serde(default)]
    pub hue: Option<In>,
    #[serde(default)]
    pub sat: Option<In>,
    #[serde(default)]
    pub val: Option<In>,
}

fn rgb_to_hsv(c: [f32; 3]) -> [f32; 3] {
    let (mx, mn) = (c[0].max(c[1]).max(c[2]), c[0].min(c[1]).min(c[2]));
    let d = mx - mn;
    let h = if d < 1e-9 {
        0.0
    } else if mx == c[0] {
        ((c[1] - c[2]) / d).rem_euclid(6.0)
    } else if mx == c[1] {
        (c[2] - c[0]) / d + 2.0
    } else {
        (c[0] - c[1]) / d + 4.0
    } / 6.0;
    [h, if mx > 0.0 { d / mx } else { 0.0 }, mx]
}

fn hsv_to_rgb(h: [f32; 3]) -> [f32; 3] {
    let (hh, s, v) = (h[0].rem_euclid(1.0) * 6.0, h[1], h[2]);
    let c = v * s;
    let x = c * (1.0 - ((hh % 2.0) - 1.0).abs());
    let (r, g, b) = match hh as u32 {
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

impl Eval for Hsv {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let a = cx.input(&self.input)?;
        let h = cx.or(&self.hue, Field::F(0.0))?;
        let s = cx.or(&self.sat, Field::F(1.0))?;
        let v = cx.or(&self.val, Field::F(1.0))?;
        Ok(vec![(
            "",
            pv3(cx.n(), |i| {
                let q = rgb_to_hsv(a.v3(i));
                hsv_to_rgb([
                    q[0] + h.f(i),
                    (q[1] * s.f(i)).clamp(0.0, 1.0),
                    q[2] * v.f(i),
                ])
            }),
        )])
    }
}

/// Layers `b` over `a`: normal (default), multiply, screen, overlay, add,
/// with `opacity` (a number or a mask field).
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Blend {
    pub a: In,
    pub b: In,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub opacity: Option<In>,
}

impl Eval for Blend {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let (a, b) = (cx.input(&self.a)?, cx.input(&self.b)?);
        let o = cx.or(&self.opacity, Field::F(1.0))?;
        let mode = self.mode.as_deref().unwrap_or("normal");
        if !matches!(mode, "normal" | "multiply" | "screen" | "overlay" | "add") {
            return Err(format!(
                "mode: `{mode}`: use normal, multiply, screen, overlay or add"
            ));
        }
        let mix = |x: f32, y: f32| match mode {
            "multiply" => x * y,
            "screen" => 1.0 - (1.0 - x) * (1.0 - y),
            "overlay" => {
                if x < 0.5 {
                    2.0 * x * y
                } else {
                    1.0 - 2.0 * (1.0 - x) * (1.0 - y)
                }
            }
            "add" => x + y,
            _ => y,
        };
        let n = cx.n();
        let scalar = a.dims() == 1 && b.dims() == 1;
        Ok(vec![(
            "",
            if scalar {
                pf(n, |i| {
                    let (x, t) = (a.f(i), o.f(i));
                    x + (mix(x, b.f(i)) - x) * t
                })
            } else {
                pv3(n, |i| {
                    let (x, y, t) = (a.v3(i), b.v3(i), o.f(i));
                    [0, 1, 2].map(|k| x[k] + (mix(x[k], y[k]) - x[k]) * t)
                })
            },
        )])
    }
}

/// Levels: input range [lo, hi] → gamma → output range [lo, hi].
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Levels {
    pub input: In,
    #[serde(default, rename = "in")]
    pub input_range: Option<[f32; 2]>,
    #[serde(default)]
    pub gamma: Option<f32>,
    #[serde(default)]
    pub out: Option<[f32; 2]>,
}

impl Eval for Levels {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let a = cx.input(&self.input)?;
        let (ir, or) = (
            self.input_range.unwrap_or([0.0, 1.0]),
            self.out.unwrap_or([0.0, 1.0]),
        );
        let g = self.gamma.unwrap_or(1.0).max(1e-3);
        let lv = move |x: f32| {
            let u = ((x - ir[0]) / (ir[1] - ir[0]).max(1e-12)).clamp(0.0, 1.0);
            or[0] + (or[1] - or[0]) * u.powf(1.0 / g)
        };
        let n = cx.n();
        Ok(vec![(
            "",
            if a.dims() == 1 {
                pf(n, |i| lv(a.f(i)))
            } else {
                pv3(n, |i| a.v3(i).map(lv))
            },
        )])
    }
}

// ---------------------------------------------------------------- neighbourhood
// These read neighbouring samples on the bake grid (wrapping in the tile
// domain, staying inside valid texels in the object domain).

/// Box blur of `radius` meters (number or colour).
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Blur {
    pub input: In,
    pub radius: f32,
}

impl Eval for Blur {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let a = cx.input(&self.input)?;
        let r = (self.radius / cx.s.texel).round().clamp(1.0, 64.0) as i64;
        let s = cx.s;
        let avg = |i: usize, get: &dyn Fn(usize) -> [f32; 3]| {
            let (mut acc, mut n) = ([0.0f32; 3], 0.0f32);
            for dy in -r..=r {
                for dx in -r..=r {
                    if let Some(j) = s.neighbour(i, dx, dy) {
                        let v = get(j);
                        acc = [acc[0] + v[0], acc[1] + v[1], acc[2] + v[2]];
                        n += 1.0;
                    }
                }
            }
            acc.map(|x| x / n.max(1.0))
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

/// Central differences of a height field, in height units per meter.
fn slope(s: &Samples, h: &Field, i: usize) -> (f32, f32) {
    let t = s.texel.max(1e-9);
    let g = |dx: i64, dy: i64| s.neighbour(i, dx, dy).map(|j| h.f(j));
    let c = h.f(i);
    let (l, r) = (g(-1, 0).unwrap_or(c), g(1, 0).unwrap_or(c));
    let (d, u) = (g(0, -1).unwrap_or(c), g(0, 1).unwrap_or(c));
    ((r - l) / (2.0 * t), (u - d) / (2.0 * t))
}

/// Tangent-space normal (vec3, z up, -1..1) from a height field in meters
/// (or any units × `strength`).
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NormalFromHeight {
    pub height: In,
    #[serde(default)]
    pub strength: Option<f32>,
}

impl Eval for NormalFromHeight {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let h = cx.input(&self.height)?;
        let k = self.strength.unwrap_or(1.0);
        Ok(vec![("", pv3(cx.n(), |i| height_normal(cx.s, &h, i, k)))])
    }
}

pub fn height_normal(s: &Samples, h: &Field, i: usize, k: f32) -> [f32; 3] {
    let (dx, dy) = slope(s, h, i);
    let n = [-dx * k, -dy * k, 1.0];
    let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    n.map(|v| v / l)
}

/// Ambient occlusion (1 = open, 0 = buried) from a height field in meters:
/// how far the neighbourhood within `radius` meters rises above the sample.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AoFromHeight {
    pub height: In,
    pub radius: f32,
    #[serde(default)]
    pub strength: Option<f32>,
}

impl Eval for AoFromHeight {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let h = cx.input(&self.height)?;
        let r = (self.radius / cx.s.texel).round().clamp(1.0, 32.0) as i64;
        let k = self.strength.unwrap_or(1.0);
        let s = cx.s;
        let rad = self.radius.max(1e-6);
        Ok(vec![(
            "",
            pf(cx.n(), |i| {
                let c = h.f(i);
                let (mut occ, mut n) = (0.0f32, 0.0f32);
                // 8 directions, a few steps each: the steepest rise wins.
                for (ux, uy) in [
                    (1, 0),
                    (-1, 0),
                    (0, 1),
                    (0, -1),
                    (1, 1),
                    (-1, -1),
                    (1, -1),
                    (-1, 1),
                ] {
                    let mut best = 0.0f32;
                    for step in 1..=r {
                        if let Some(j) = s.neighbour(i, ux * step, uy * step) {
                            let dist = s.texel
                                * step as f32
                                * if ux != 0 && uy != 0 { 1.414 } else { 1.0 };
                            best = best.max((h.f(j) - c) / dist);
                        }
                    }
                    occ += best.atan() / std::f32::consts::FRAC_PI_2;
                    n += 1.0;
                }
                let _ = rad;
                (1.0 - k * occ / n.max(1.0)).clamp(0.0, 1.0)
            }),
        )])
    }
}

/// Curvature of a height field: > 0.5 on ridges and convex edges (wear),
/// < 0.5 in creases (dirt), scaled by `strength`.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Curvature {
    pub height: In,
    #[serde(default)]
    pub strength: Option<f32>,
}

impl Eval for Curvature {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let h = cx.input(&self.height)?;
        let k = self.strength.unwrap_or(1.0);
        let s = cx.s;
        let t = s.texel.max(1e-9);
        Ok(vec![(
            "",
            pf(cx.n(), |i| {
                let c = h.f(i);
                let g = |dx: i64, dy: i64| s.neighbour(i, dx, dy).map_or(c, |j| h.f(j));
                let lap = (g(1, 0) + g(-1, 0) + g(0, 1) + g(0, -1) - 4.0 * c) / (t * t);
                (0.5 - lap * k * t).clamp(0.0, 1.0)
            }),
        )])
    }
}

/// Grows (`radius` > 0, max filter) or shrinks (< 0, min filter) a number
/// field by |radius| meters.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Dilate {
    pub input: In,
    pub radius: f32,
}

impl Eval for Dilate {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let a = cx.input(&self.input)?;
        let r = (self.radius.abs() / cx.s.texel).round().clamp(1.0, 32.0) as i64;
        let grow = self.radius >= 0.0;
        let s = cx.s;
        Ok(vec![(
            "",
            pf(cx.n(), |i| {
                let mut v = a.f(i);
                for dy in -r..=r {
                    for dx in -r..=r {
                        if dx * dx + dy * dy > r * r {
                            continue;
                        }
                        if let Some(j) = s.neighbour(i, dx, dy) {
                            v = if grow { v.max(a.f(j)) } else { v.min(a.f(j)) };
                        }
                    }
                }
                v
            }),
        )])
    }
}

// ---------------------------------------------------------------- output

/// The material's channels. Each is a constant or a node; missing channels
/// use defaults (base 0.8 grey, roughness 0.5, metallic 0, flat normal,
/// ao 1, no emission, opaque). `normal` may be omitted when `height` is
/// given: it is derived from the height (× `normal_strength`).
#[derive(Clone, Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Output {
    #[serde(default)]
    pub base_color: Option<In>,
    #[serde(default)]
    pub roughness: Option<In>,
    #[serde(default)]
    pub metallic: Option<In>,
    /// Height in meters (relief), drives the derived normal.
    #[serde(default)]
    pub height: Option<In>,
    /// Tangent-space normal (vec3, -1..1, z up).
    #[serde(default)]
    pub normal: Option<In>,
    #[serde(default)]
    pub normal_strength: Option<f32>,
    #[serde(default)]
    pub ao: Option<In>,
    #[serde(default)]
    pub emissive: Option<In>,
    #[serde(default)]
    pub opacity: Option<In>,
}

impl Eval for Output {
    fn eval(&self, _: &Cx) -> Result<Outs, String> {
        Ok(Vec::new())
    }
}

/// Node references in a node body: every string that is not a colour and
/// not a mode/kind/op word, as (node, port).
pub fn refs(body: &Value) -> Vec<(String, String)> {
    const WORDS: &[&str] = &["type", "name", "op", "mode", "kind", "of", "out"];
    let mut out = Vec::new();
    fn walk(v: &Value, key: Option<&str>, out: &mut Vec<(String, String)>) {
        match v {
            Value::String(s) => {
                if key.is_some_and(|k| WORDS.contains(&k)) || s.starts_with('#') {
                    return;
                }
                let (n, p) = s.split_once('.').unwrap_or((s, ""));
                out.push((n.to_string(), p.to_string()));
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, key, out)),
            Value::Object(m) => m.iter().for_each(|(k, x)| walk(x, Some(k), out)),
            _ => {}
        }
    }
    walk(body, None, &mut out);
    out
}
