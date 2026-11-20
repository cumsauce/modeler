//! Hair, eyebrows and eyelashes of the human preset.
//!
//! All three are built after the body, from its landmarks and zones, as
//! card parts whose every vertex is tied to one body vertex (its anchor):
//! the card takes that vertex's skin weights and morph deltas, so poses,
//! `shapes` morphs and skinning carry it, and because the positions are
//! rebuilt from the evaluated body, expressions (a blink, a brow raise)
//! move them too.
//!
//! - `<name>_brows`: a tapered strip on each brow curve, drawn with an
//!   alpha-cutout hair texture.
//! - `<name>_lashes`: a curled strip along each upper and lower lid margin.
//! - `<name>_hair`: a scalp cap (the scalp zone of the body pushed out a
//!   little, its hairline feathered by the cutout so it never shows a rim)
//!   plus layered strand cards grown from the scalp along a groom field
//!   (away from the part, then gravity; curls by style), kept outside the
//!   head and off the shoulders; afro / bun / curly add a displaced volume.

use std::collections::HashMap;
use std::sync::Arc;

use bevy::math::Vec3;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::landmarks::Landmarks;
use super::look::{ColorIn, hex};
use crate::Part;

pub const STYLES: &[&str] = &[
    "bald", "buzz", "short", "crop", "bob", "long", "ponytail", "bun", "afro", "curly",
];
/// The style a human gets without a `hair` field.
pub const DEFAULT_STYLE: &str = "short";
pub const DEFAULT_COLOR: [f32; 3] = [0.33, 0.22, 0.14];

/// `hair` on a human body item.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HairDef {
    /// bald, buzz, short (default), crop, bob, long, ponytail, bun, afro,
    /// curly.
    #[serde(default)]
    pub style: Option<String>,
    /// Strand length in meters (default by style).
    #[serde(default)]
    pub length: Option<f32>,
    #[serde(default)]
    pub color: Option<ColorIn>,
    /// left, right, middle or none (default by style).
    #[serde(default)]
    pub part: Option<String>,
    /// 0..1: how much of the front falls over the forehead.
    #[serde(default)]
    pub fringe: Option<f32>,
    /// 0..1: how far the hair stands off the head.
    #[serde(default)]
    pub volume: Option<f32>,
    /// Card count (default by style, 40..1500).
    #[serde(default)]
    pub cards: Option<u32>,
    /// Triangle budget: cards are dropped to fit.
    #[serde(default)]
    pub tris: Option<u32>,
    #[serde(default)]
    pub seed: Option<u32>,
}

/// `hair`: a style name or the full object.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(untagged)]
pub enum HairIn {
    Style(String),
    Def(HairDef),
}

impl HairIn {
    pub fn def(&self) -> HairDef {
        match self {
            HairIn::Style(s) => HairDef {
                style: Some(s.clone()),
                ..HairDef::default()
            },
            HairIn::Def(d) => d.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BrowsDef {
    /// Height of the brow at its peak, meters (default 0.008).
    #[serde(default)]
    pub thickness: Option<f32>,
    /// -1..1: flatter .. higher arch (default 0).
    #[serde(default)]
    pub arch: Option<f32>,
    /// Default: the hair colour, a little darker.
    #[serde(default)]
    pub color: Option<ColorIn>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LashesDef {
    /// Upper lash length, meters (default 0.009).
    #[serde(default)]
    pub length: Option<f32>,
    /// 0..1 (default 0.5).
    #[serde(default)]
    pub curl: Option<f32>,
    #[serde(default)]
    pub color: Option<ColorIn>,
}

/// `brows` / `lashes`: `false` turns them off, an object tunes them.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(untagged)]
pub enum Toggle<T> {
    On(bool),
    Def(T),
}

impl<T: Default + Clone> Toggle<T> {
    pub fn get(t: &Option<Toggle<T>>) -> Option<T> {
        match t {
            None | Some(Toggle::On(true)) => Some(T::default()),
            Some(Toggle::On(false)) => None,
            Some(Toggle::Def(d)) => Some(d.clone()),
        }
    }
}

/// The hair style of a body item (`bald` = no hair part).
pub fn style_of(def: &crate::body::BodyDef) -> String {
    def.hair
        .as_ref()
        .map(|h| h.def())
        .and_then(|d| d.style)
        .unwrap_or_else(|| DEFAULT_STYLE.to_string())
}

/// The extra part names (suffixes) hair, brows and lashes add.
pub fn outputs(def: &crate::body::BodyDef) -> Vec<&'static str> {
    let mut v = Vec::new();
    if Toggle::get(&def.brows).is_some() {
        v.push("brows");
    }
    if Toggle::get(&def.lashes).is_some() {
        v.push("lashes");
    }
    match style_of(def).as_str() {
        "bald" => {}
        "buzz" => v.push("hair"),
        _ => v.extend(["hair", "hair_cap"]),
    }
    v
}

pub fn validate(def: &crate::body::BodyDef) -> Result<(), String> {
    if let Some(h) = &def.hair {
        let d = h.def();
        let s = d.style.clone().unwrap_or_else(|| DEFAULT_STYLE.into());
        if !STYLES.contains(&s.as_str()) {
            return Err(format!(
                "hair.style: `{s}` is not one of {}",
                STYLES.join(", ")
            ));
        }
        if let Some(p) = &d.part
            && !["left", "right", "middle", "none"].contains(&p.as_str())
        {
            return Err(format!("hair.part: `{p}`: use left, right, middle or none"));
        }
        for (k, v, lo, hi) in [
            ("length", d.length, 0.002, 1.5),
            ("fringe", d.fringe, 0.0, 1.0),
            ("volume", d.volume, 0.0, 1.0),
        ] {
            if let Some(x) = v
                && (!x.is_finite() || x < lo || x > hi)
            {
                return Err(format!("hair.{k}: {lo}..{hi}, got {x}"));
            }
        }
        if let Some(c) = d.cards
            && !(40..=1500).contains(&c)
        {
            return Err(format!("hair.cards: 40..1500, got {c}"));
        }
        if let Some(c) = &d.color {
            c.rgb().map_err(|e| format!("hair.color: {e}"))?;
        }
    }
    if let Some(Toggle::Def(b)) = &def.brows {
        if let Some(c) = &b.color {
            c.rgb().map_err(|e| format!("brows.color: {e}"))?;
        }
        if b.arch.is_some_and(|a| !(-1.0..=1.0).contains(&a)) {
            return Err("brows.arch: -1..1".into());
        }
        if b.thickness.is_some_and(|t| !(0.001..=0.03).contains(&t)) {
            return Err("brows.thickness: 0.001..0.03 m".into());
        }
    }
    if let Some(Toggle::Def(l)) = &def.lashes {
        if let Some(c) = &l.color {
            c.rgb().map_err(|e| format!("lashes.color: {e}"))?;
        }
        if l.length.is_some_and(|t| !(0.001..=0.03).contains(&t)) {
            return Err("lashes.length: 0.001..0.03 m".into());
        }
        if l.curl.is_some_and(|a| !(0.0..=1.0).contains(&a)) {
            return Err("lashes.curl: 0..1".into());
        }
    }
    if let Some(t) = &def.skin_tone {
        t.rgb()?;
    }
    Ok(())
}

// ------------------------------------------------------------------ body

/// The body part as a lookup: nearest vertex, normals.
struct Body<'a> {
    part: &'a Part,
    nrm: Vec<Vec3>,
    grid: HashMap<[i32; 3], Vec<u32>>,
    cell: f32,
}

impl<'a> Body<'a> {
    fn new(part: &'a Part) -> Self {
        let cell = 0.02;
        let mut grid: HashMap<[i32; 3], Vec<u32>> = HashMap::new();
        for (i, v) in part.verts.iter().enumerate() {
            grid.entry(key(Vec3::from(*v), cell))
                .or_default()
                .push(i as u32);
        }
        let nrm = part
            .normals
            .iter()
            .map(|n| Vec3::from(*n).normalize_or(Vec3::Y))
            .collect();
        Body {
            part,
            nrm,
            grid,
            cell,
        }
    }
    fn p(&self, i: u32) -> Vec3 {
        Vec3::from(self.part.verts[i as usize])
    }
    fn nearest(&self, q: Vec3) -> u32 {
        let k = key(q, self.cell);
        let mut best = (f32::MAX, 0u32);
        for reach in 1..8 {
            for dx in -reach..=reach {
                for dy in -reach..=reach {
                    for dz in -reach..=reach {
                        if let Some(v) = self.grid.get(&[k[0] + dx, k[1] + dy, k[2] + dz]) {
                            for &i in v {
                                let d = self.p(i).distance_squared(q);
                                if d < best.0 {
                                    best = (d, i);
                                }
                            }
                        }
                    }
                }
            }
            if best.0 < f32::MAX {
                break;
            }
        }
        best.1
    }
    /// Pushes `q` out of the body to `clear` meters off its surface
    /// (nearest-vertex test; good for the smooth head, neck and back).
    fn push_out(&self, q: Vec3, clear: f32) -> Vec3 {
        let i = self.nearest(q);
        let (v, n) = (self.p(i), self.nrm[i as usize]);
        let h = (q - v).dot(n);
        if h < clear && (q - v).length() < 0.06 {
            q + n * (clear - h)
        } else {
            q
        }
    }
}

fn key(v: Vec3, cell: f32) -> [i32; 3] {
    [
        (v.x / cell).floor() as i32,
        (v.y / cell).floor() as i32,
        (v.z / cell).floor() as i32,
    ]
}

/// Card geometry with one body anchor per vertex.
#[derive(Default)]
struct Cards {
    v: Vec<Vec3>,
    n: Vec<Vec3>,
    uv: Vec<[f32; 2]>,
    idx: Vec<u32>,
    anchor: Vec<u32>,
}

impl Cards {
    /// A strip through `centre` points, half-width vectors `side`, card
    /// normal `nrm` per point; uv u across (0..1), v along (v0..v1).
    fn strip(
        &mut self,
        centre: &[Vec3],
        side: &[Vec3],
        nrm: &[Vec3],
        anchor: &[u32],
        v0: f32,
        v1: f32,
    ) {
        let m = centre.len();
        if m < 2 {
            return;
        }
        let base = self.v.len() as u32;
        for i in 0..m {
            let t = i as f32 / (m - 1) as f32;
            let v = v0 + (v1 - v0) * t;
            for (s, u) in [(-1.0f32, 0.0f32), (1.0, 1.0)] {
                self.v.push(centre[i] + side[i] * s);
                self.n.push(nrm[i]);
                self.uv.push([u, v]);
                self.anchor.push(anchor[i.min(anchor.len() - 1)]);
            }
        }
        for i in 0..m as u32 - 1 {
            let (a, b, c, d) = (
                base + 2 * i,
                base + 2 * i + 1,
                base + 2 * i + 2,
                base + 2 * i + 3,
            );
            self.idx.extend([a, c, b, b, c, d]);
        }
    }

    fn into_part(
        self,
        name: &str,
        body: &Part,
        graph: crate::material::MatGraph,
        mat: &str,
    ) -> Part {
        let mut p = Part {
            name: name.to_string(),
            verts: self.v.iter().map(|v| v.to_array()).collect(),
            normals: self
                .n
                .iter()
                .map(|n| n.normalize_or(Vec3::Y).to_array())
                .collect(),
            uvs: self.uv,
            indices: self.idx,
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0; 3],
            visible: true,
            cards: true,
            ..Part::default()
        };
        if let Some(s) = &body.skin {
            p.skin = Some(Arc::new(super::SkinWeights {
                bones: s.bones.clone(),
                joints: self.anchor.iter().map(|&i| s.joints[i as usize]).collect(),
                weights: self.anchor.iter().map(|&i| s.weights[i as usize]).collect(),
            }));
        }
        if let Some(m) = &body.morphs {
            p.morphs = Some(Arc::new(
                m.iter()
                    .map(|(n, d)| {
                        (
                            n.clone(),
                            self.anchor.iter().map(|&i| d[i as usize]).collect(),
                        )
                    })
                    .collect(),
            ));
        }
        p.material.base_color = [1.0; 4];
        p.material.roughness = 1.0;
        p.material.metalness = 0.0;
        p.material.pbr = Some(crate::material::PbrRef {
            material: mat.into(),
            params: Default::default(),
            scale: 1.0,
            graph: Some(Arc::new(graph)),
        });
        p
    }
}

/// A tiny deterministic RNG.
struct Rng(u64);
impl Rng {
    fn f(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
}

// ------------------------------------------------------------- materials

/// Strand cards: fine streaks along v, tapering at the tips (v 1) and at
/// the card's long edges, root darker than tip, anisotropic highlight.
pub fn strand_graph(
    color: [f32; 3],
    fine: f32,
    coverage: f32,
    seed: u32,
) -> crate::material::MatGraph {
    let k = |f: f32| hex([color[0] * f, color[1] * f, color[2] * f]);
    crate::material::MatGraph {
        domain: "card".into(),
        tile: 1.0,
        resolution: 256,
        nodes: vec![
            json!({"type": "transform", "name": "st", "scale": [fine, 1.5]}),
            json!({"type": "noise", "name": "streak", "coords": "st", "size": 1.0, "octaves": 2, "seed": seed}),
            json!({"type": "transform", "name": "st2", "scale": [fine * 0.37, 3.0]}),
            json!({"type": "noise", "name": "streak2", "coords": "st2", "size": 1.0, "octaves": 1, "seed": seed + 9}),
            json!({"type": "math", "name": "strands", "op": "mix", "a": "streak", "b": "streak2", "c": 0.35}),
            json!({"type": "input", "name": "uv"}),
            json!({"type": "split", "name": "uvs", "input": "uv"}),
            json!({"type": "math", "name": "du", "op": "sub", "a": "uvs.x", "b": 0.5}),
            json!({"type": "math", "name": "adu", "op": "abs", "a": "du"}),
            json!({"type": "math", "name": "edge", "op": "smoothstep", "a": "adu", "b": 0.5, "c": 0.15}),
            json!({"type": "math", "name": "tip", "op": "smoothstep", "a": "uvs.y", "b": 1.0, "c": 0.55}),
            json!({"type": "math", "name": "env", "op": "mul", "a": "edge", "b": "tip"}),
            // Thinner strands toward the tip: the threshold rises with v.
            json!({"type": "math", "name": "thr0", "op": "mul", "a": "env", "b": coverage * 0.8}),
            json!({"type": "math", "name": "thr", "op": "sub", "a": 1.0, "b": "thr0"}),
            json!({"type": "math", "name": "hi", "op": "add", "a": "thr", "b": 0.08}),
            json!({"type": "math", "name": "alpha0", "op": "smoothstep", "a": "strands", "b": "thr", "c": "hi"}),
            // The cap's strip (v < 0.03): opaque, fading toward the hairline.
            json!({"type": "math", "name": "capv", "op": "smoothstep", "a": "uvs.y", "b": 0.035, "c": 0.025}),
            json!({"type": "math", "name": "jit", "op": "mul", "a": "streak", "b": 0.2}),
            json!({"type": "math", "name": "capu0", "op": "add", "a": "adu", "b": "jit"}),
            json!({"type": "math", "name": "capu", "op": "smoothstep", "a": "capu0", "b": 0.52, "c": 0.4}),
            json!({"type": "math", "name": "cap", "op": "mul", "a": "capv", "b": "capu"}),
            json!({"type": "math", "name": "alpha", "op": "max", "a": "alpha0", "b": "cap"}),
            json!({"type": "ramp", "name": "along", "input": "uvs.y",
                   "stops": [[0.0, k(0.55)], [0.35, k(0.9)], [1.0, k(1.2)]]}),
            json!({"type": "math", "name": "var", "op": "mix", "a": 0.75, "b": 1.15, "c": "streak2"}),
            json!({"type": "math", "name": "col", "op": "mul", "a": "along", "b": "var"}),
            json!({"type": "math", "name": "relief", "op": "mul", "a": "strands", "b": 0.0004}),
            json!({"type": "output", "name": "out", "base_color": "col", "roughness": 0.7, "height": "relief",
                   "opacity": "alpha", "alpha": "mask", "alpha_cutoff": 0.5,
                   "anisotropy": 0.2, "anisotropy_rotation": 90.0}),
        ],
        params: Default::default(),
    }
}

/// The scalp cap: hair-coloured streaks, opaque inside, broken up by
/// noise toward the hairline (v 1) so the edge reads as hair, not a rim.
pub fn cap_graph(color: [f32; 3], short: bool) -> crate::material::MatGraph {
    let k = |f: f32| hex([color[0] * f, color[1] * f, color[2] * f]);
    let (lo, hi) = if short { (0.25, 0.95) } else { (0.45, 1.0) };
    crate::material::MatGraph {
        domain: "card".into(),
        tile: 1.0,
        resolution: 512,
        nodes: vec![
            json!({"type": "input", "name": "uv"}),
            json!({"type": "split", "name": "uvs", "input": "uv"}),
            json!({"type": "noise", "name": "grain", "size": 0.004, "octaves": 2, "seed": 3}),
            json!({"type": "noise", "name": "clump", "size": 0.03, "octaves": 3, "seed": 8}),
            json!({"type": "math", "name": "n", "op": "mix", "a": "grain", "b": "clump", "c": 0.4}),
            json!({"type": "math", "name": "jit", "op": "mul", "a": "n", "b": 0.45}),
            json!({"type": "math", "name": "e", "op": "add", "a": "uvs.y", "b": "jit"}),
            json!({"type": "math", "name": "alpha", "op": "smoothstep", "a": "e", "b": hi + 0.2, "c": lo + 0.2}),
            json!({"type": "ramp", "name": "col", "input": "grain",
                   "stops": [[0.0, k(0.4)], [1.0, k(0.8)]]}),
            json!({"type": "output", "name": "out", "base_color": "col", "roughness": 0.9,
                   "opacity": "alpha", "alpha": "mask", "alpha_cutoff": 0.5}),
        ],
        params: Default::default(),
    }
}

/// Brows: short hairs slanting along the brow (u inner -> outer, v up).
pub fn brow_graph(color: [f32; 3]) -> crate::material::MatGraph {
    let k = |f: f32| hex([color[0] * f, color[1] * f, color[2] * f]);
    crate::material::MatGraph {
        domain: "card".into(),
        tile: 1.0,
        resolution: 256,
        nodes: vec![
            json!({"type": "input", "name": "uv"}),
            json!({"type": "split", "name": "uvs", "input": "uv"}),
            json!({"type": "transform", "name": "slant", "rotate": 55.0, "scale": [1.0, 1.0]}),
            json!({"type": "transform", "name": "st", "coords": "slant", "scale": [8.0, 45.0]}),
            json!({"type": "noise", "name": "hairs", "coords": "st", "size": 1.0, "octaves": 2, "seed": 21}),
            // Envelope: thick in the middle, thin at the outer tail.
            json!({"type": "math", "name": "dv", "op": "sub", "a": "uvs.y", "b": 0.5}),
            json!({"type": "math", "name": "adv", "op": "abs", "a": "dv"}),
            json!({"type": "math", "name": "half", "op": "mix", "a": 0.5, "b": 0.3, "c": "uvs.x"}),
            json!({"type": "math", "name": "half_in", "op": "mul", "a": "half", "b": 0.7}),
            json!({"type": "math", "name": "vin", "op": "smoothstep", "a": "adv", "b": "half", "c": "half_in"}),
            json!({"type": "math", "name": "uin", "op": "smoothstep", "a": "uvs.x", "b": 0.0, "c": 0.12}),
            json!({"type": "math", "name": "uout", "op": "smoothstep", "a": "uvs.x", "b": 1.0, "c": 0.85}),
            json!({"type": "math", "name": "e1", "op": "mul", "a": "vin", "b": "uin"}),
            json!({"type": "math", "name": "env", "op": "mul", "a": "e1", "b": "uout"}),
            json!({"type": "math", "name": "thr0", "op": "mul", "a": "env", "b": 0.8}),
            json!({"type": "math", "name": "thr", "op": "sub", "a": 1.0, "b": "thr0"}),
            json!({"type": "math", "name": "hi", "op": "add", "a": "thr", "b": 0.1}),
            json!({"type": "math", "name": "alpha", "op": "smoothstep", "a": "hairs", "b": "thr", "c": "hi"}),
            json!({"type": "ramp", "name": "col", "input": "hairs", "stops": [[0.0, k(0.6)], [1.0, k(1.0)]]}),
            json!({"type": "output", "name": "out", "base_color": "col", "roughness": 0.75,
                   "opacity": "alpha", "alpha": "mask", "alpha_cutoff": 0.5}),
        ],
        params: Default::default(),
    }
}

/// Lashes: dark strands along v (root 0 -> tip 1), u along the lid.
pub fn lash_graph(color: [f32; 3]) -> crate::material::MatGraph {
    let c = hex(color);
    crate::material::MatGraph {
        domain: "card".into(),
        tile: 1.0,
        resolution: 256,
        nodes: vec![
            json!({"type": "input", "name": "uv"}),
            json!({"type": "split", "name": "uvs", "input": "uv"}),
            json!({"type": "transform", "name": "st", "scale": [90.0, 1.2]}),
            json!({"type": "noise", "name": "lash", "coords": "st", "size": 1.0, "octaves": 1, "seed": 31}),
            json!({"type": "math", "name": "tip", "op": "smoothstep", "a": "uvs.y", "b": 1.0, "c": 0.2}),
            json!({"type": "math", "name": "uin", "op": "smoothstep", "a": "uvs.x", "b": 0.0, "c": 0.15}),
            json!({"type": "math", "name": "uout", "op": "smoothstep", "a": "uvs.x", "b": 1.0, "c": 0.8}),
            json!({"type": "math", "name": "e1", "op": "mul", "a": "tip", "b": "uin"}),
            json!({"type": "math", "name": "env", "op": "mul", "a": "e1", "b": "uout"}),
            json!({"type": "math", "name": "thr0", "op": "mul", "a": "env", "b": 0.6}),
            json!({"type": "math", "name": "thr", "op": "sub", "a": 1.0, "b": "thr0"}),
            json!({"type": "math", "name": "hi", "op": "add", "a": "thr", "b": 0.06}),
            json!({"type": "math", "name": "alpha", "op": "smoothstep", "a": "lash", "b": "thr", "c": "hi"}),
            json!({"type": "output", "name": "out", "base_color": c, "roughness": 0.4,
                   "opacity": "alpha", "alpha": "mask", "alpha_cutoff": 0.5}),
        ],
        params: Default::default(),
    }
}

// ------------------------------------------------------------------ build

/// Builds brows, lashes and hair for a human item from its body part and
/// the evaluated template positions (`cage`, same space as the body).
pub fn build(
    name: &str,
    def: &crate::body::BodyDef,
    body: &Part,
    cage: &[Vec3],
    lm: &Landmarks,
) -> Result<Vec<Part>, String> {
    let b = Body::new(body);
    let hair = def.hair.as_ref().map(|h| h.def()).unwrap_or_default();
    let hair_col = match &hair.color {
        Some(c) => c.rgb()?,
        None => DEFAULT_COLOR,
    };
    // Template vertex -> body vertex.
    let at = |v: u32| -> u32 { b.nearest(cage[v as usize]) };
    let mut out = Vec::new();
    if let Some(bd) = Toggle::get(&def.brows) {
        let col = match &bd.color {
            Some(c) => c.rgb()?,
            None => hair_col.map(|x| x * 0.8),
        };
        let mut cards = Cards::default();
        for s in ["l", "r"] {
            let key = ["inner", "peak", "outer"]
                .map(|k| lm.point(&format!("brow_{k}_{s}")).map(|v| b.p(at(v))));
            let [Some(k0), Some(k1), Some(k2)] = key else {
                continue;
            };
            brow_strip(&mut cards, &b, [k0, k1, k2], &bd);
        }
        out.push(cards.into_part(
            &format!("{name}_brows"),
            body,
            brow_graph(col),
            "human_brows",
        ));
    }
    if let Some(ld) = Toggle::get(&def.lashes) {
        let col = match &ld.color {
            Some(c) => c.rgb()?,
            None => [0.05, 0.04, 0.035],
        };
        let mut cards = Cards::default();
        for s in ["l", "r"] {
            for (lid, k) in [("upper", 1.0f32), ("lower", 0.45)] {
                let Some(curve) = lm.curve(&format!("lid_{lid}_{s}")) else {
                    continue;
                };
                lash_strip(&mut cards, &b, curve, &at, &ld, k, lid == "upper");
            }
        }
        out.push(cards.into_part(
            &format!("{name}_lashes"),
            body,
            lash_graph(col),
            "human_lashes",
        ));
    }
    let style = hair.style.clone().unwrap_or_else(|| DEFAULT_STYLE.into());
    if style != "bald" {
        out.extend(build_hair(
            name, &hair, &style, hair_col, body, &b, cage, lm,
        )?);
    }
    Ok(out)
}

fn brow_strip(cards: &mut Cards, b: &Body, key: [Vec3; 3], d: &BrowsDef) {
    // A smooth curve through inner, peak and outer, laid on the skin (each
    // sample projected onto its nearest vertex's tangent plane).
    let m = 13usize;
    let th = d.thickness.unwrap_or(0.010);
    let arch = d.arch.unwrap_or(0.0);
    let mut c = Vec::new();
    let mut side = Vec::new();
    let mut nrm = Vec::new();
    let mut anchors = Vec::new();
    for i in 0..m {
        let t = i as f32 / (m - 1) as f32;
        // Quadratic Bezier with its control point set so it passes the peak
        // at t = 0.5.
        let ctrl = key[1] * 2.0 - (key[0] + key[2]) * 0.5;
        let q = key[0] * (1.0 - t) * (1.0 - t) + ctrl * 2.0 * t * (1.0 - t) + key[2] * t * t;
        let lift = arch * 0.004 * (std::f32::consts::PI * t).sin();
        let q = q + Vec3::Y * lift;
        let a = b.nearest(q);
        let (v, n) = (b.p(a), b.nrm[a as usize]);
        let on = q - n * (q - v).dot(n) + n * 0.0009;
        let up = (Vec3::Y - n * n.y).normalize_or(Vec3::Y);
        // Taper: full at the inner two thirds, thin at the tail.
        let w =
            th * (0.85 + 0.15 * (t * 3.0).min(1.0)) * (1.0 - 0.55 * ((t - 0.55) / 0.45).max(0.0));
        c.push(on + up * (w * 0.1));
        side.push(up * (w * 0.5));
        nrm.push(n);
        anchors.push(a);
    }
    // The strip's u runs across (side), v along; the brow texture wants u
    // along: build with the axes swapped by emitting it as columns.
    let base = cards.v.len() as u32;
    for i in 0..m {
        let t = i as f32 / (m - 1) as f32;
        for (s, v) in [(-1.0f32, 0.0f32), (1.0, 1.0)] {
            cards.v.push(c[i] + side[i] * s);
            cards.n.push(nrm[i]);
            cards.uv.push([t, 1.0 - v]);
            cards.anchor.push(anchors[i]);
        }
    }
    for i in 0..m as u32 - 1 {
        let (a0, b0, c0, d0) = (
            base + 2 * i,
            base + 2 * i + 1,
            base + 2 * i + 2,
            base + 2 * i + 3,
        );
        cards.idx.extend([a0, c0, b0, b0, c0, d0]);
    }
}

fn lash_strip(
    cards: &mut Cards,
    b: &Body,
    curve: &[u32],
    at: &dyn Fn(u32) -> u32,
    d: &LashesDef,
    k: f32,
    upper: bool,
) {
    let anchors: Vec<u32> = curve.iter().map(|&v| at(v)).collect();
    let m = anchors.len();
    if m < 3 {
        return;
    }
    let len = d.length.unwrap_or(0.009) * k;
    let curl = d.curl.unwrap_or(0.5);
    let pts: Vec<Vec3> = anchors.iter().map(|&a| b.p(a)).collect();
    // The eye's centre: behind the margin's middle.
    let mid = pts.iter().copied().sum::<Vec3>() / m as f32;
    let centre = mid - Vec3::Z * 0.011;
    let vert = if upper { Vec3::Y } else { Vec3::NEG_Y };
    let base = cards.v.len() as u32;
    let rows = 4;
    for (i, &p) in pts.iter().enumerate() {
        let t = i as f32 / (m - 1) as f32;
        // Shorter at the corners.
        let l = len * (0.45 + 0.55 * (std::f32::consts::PI * t).sin());
        let out = (p - centre).normalize_or(Vec3::Z);
        let along = (pts[(i + 1).min(m - 1)] - pts[i.saturating_sub(1)]).normalize_or(Vec3::X);
        let d0 = (out * 0.9 + vert * 0.25).normalize_or(out);
        let d1 = (out * (0.6 - 0.5 * curl) + vert * (0.5 + 0.6 * curl)).normalize_or(out);
        let mut q = p + out * 0.0006;
        for r in 0..rows {
            let s = r as f32 / (rows - 1) as f32;
            let n = along.cross(d0.lerp(d1, s)).normalize_or(out);
            cards.v.push(q);
            cards.n.push(n);
            cards.uv.push([t, s]);
            cards.anchor.push(anchors[i]);
            q += d0.lerp(d1, s).normalize_or(out) * (l / (rows - 1) as f32);
        }
    }
    let r = rows as u32;
    for i in 0..m as u32 - 1 {
        for j in 0..r - 1 {
            let a0 = base + i * r + j;
            let b0 = a0 + 1;
            let c0 = a0 + r;
            let d0 = c0 + 1;
            cards.idx.extend([a0, c0, b0, b0, c0, d0]);
        }
    }
}

/// The face, kept clear of hair: |x| < half_x, below the brow line
/// (`top_y`, a little above the brows) and above the chin, in front of
/// `z0`.
struct FaceBox {
    half_x: f32,
    top_y: f32,
    brow_y: f32,
    bottom_y: f32,
    z0: f32,
}

impl FaceBox {
    fn inside(&self, q: Vec3) -> bool {
        q.x.abs() < self.half_x && q.y < self.top_y && q.y > self.bottom_y && q.z > self.z0
    }
}

struct StyleCfg {
    length: f32,
    cards: u32,
    curl: f32,
    part: &'static str,
    fringe: f32,
    volume: f32,
    gravity: f32,
    width: f32,
    segs: usize,
}

fn cfg(style: &str) -> StyleCfg {
    let c = |length, cards, curl, part, fringe, volume, gravity, width, segs| StyleCfg {
        length,
        cards,
        curl,
        part,
        fringe,
        volume,
        gravity,
        width,
        segs,
    };
    match style {
        "buzz" => c(0.004, 0, 0.0, "none", 0.0, 0.0, 0.0, 0.0, 0),
        "short" => c(0.065, 520, 0.05, "left", 0.25, 0.3, 0.45, 0.014, 4),
        "crop" => c(0.035, 460, 0.0, "none", 0.15, 0.25, 0.2, 0.012, 3),
        "bob" => c(0.22, 520, 0.05, "middle", 0.5, 0.35, 0.9, 0.028, 7),
        "long" => c(0.45, 600, 0.08, "middle", 0.2, 0.3, 1.0, 0.03, 10),
        "ponytail" => c(0.30, 420, 0.03, "none", 0.0, 0.1, 1.0, 0.025, 8),
        "bun" => c(0.10, 340, 0.0, "none", 0.0, 0.1, 0.3, 0.022, 5),
        "afro" => c(0.06, 240, 1.0, "none", 0.0, 1.0, 0.0, 0.03, 3),
        _ => c(0.16, 480, 0.8, "none", 0.3, 0.7, 0.6, 0.026, 7), // curly
    }
}

#[allow(clippy::too_many_arguments)]
fn build_hair(
    name: &str,
    hair: &HairDef,
    style: &str,
    col: [f32; 3],
    body: &Part,
    b: &Body,
    cage: &[Vec3],
    lm: &Landmarks,
) -> Result<Vec<Part>, String> {
    let c = cfg(style);
    let length = hair.length.unwrap_or(c.length);
    let volume = hair.volume.unwrap_or(c.volume);
    let fringe = hair.fringe.unwrap_or(c.fringe);
    let part_side = hair.part.clone().unwrap_or_else(|| c.part.to_string());
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ (u64::from(hair.seed.unwrap_or(1)) * 0x2545_f491));
    // Scalp weight per body vertex (from the nearest template vertex).
    let zone = lm.zone("scalp").ok_or("no scalp zone")?;
    let mut tgrid: HashMap<[i32; 3], Vec<u32>> = HashMap::new();
    let zl: Vec<u32> = (0..zone.len() as u32)
        .filter(|&i| zone[i as usize] > 0.0)
        .collect();
    for &i in &zl {
        tgrid
            .entry(key(cage[i as usize], 0.01))
            .or_default()
            .push(i);
    }
    let w: Vec<f32> = body
        .verts
        .iter()
        .map(|v| {
            let q = Vec3::from(*v);
            let k = key(q, 0.01);
            let mut best = (0.006f32 * 0.006, 0.0f32);
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        if let Some(l) = tgrid.get(&[k[0] + dx, k[1] + dy, k[2] + dz]) {
                            for &i in l {
                                let d = cage[i as usize].distance_squared(q);
                                if d < best.0 {
                                    best = (d, zone[i as usize]);
                                }
                            }
                        }
                    }
                }
            }
            best.1
        })
        .collect();
    let crown = lm
        .point("crown")
        .map(|v| cage[v as usize])
        .unwrap_or(Vec3::new(0.0, 1.7, 0.0));
    let (mut lo, mut hi) = (Vec3::MAX, Vec3::MIN);
    for (i, v) in body.verts.iter().enumerate() {
        if w[i] > 0.5 {
            lo = lo.min(Vec3::from(*v));
            hi = hi.max(Vec3::from(*v));
        }
    }
    if lo.x > hi.x {
        return Err("hair: the scalp zone is empty".into());
    }
    let centre = Vec3::new(
        (lo.x + hi.x) * 0.5,
        crown.y - (hi.x - lo.x) * 0.5,
        (lo.z + hi.z) * 0.5,
    );
    let mut cards = Cards::default();
    // ---- cap.
    let short = matches!(style, "buzz" | "crop" | "afro");
    let lift = 0.0012;
    // Afro and curly hair: the cap pushed out into a thick, lumpy mass
    // (tapering to the hairline), dressed with short curly cards.
    let shell = match style {
        "afro" => 0.055 * (0.6 + 0.8 * volume),
        "curly" => 0.016 * (0.5 + volume),
        _ => 0.0,
    };
    let lump = |p: Vec3| {
        ((p.x * 47.0).sin() + (p.y * 39.0 + p.z * 11.0).sin() + (p.z * 43.0 - p.x * 7.0).sin())
            / 3.0
    };
    // Distance to the hairline, for a gentle taper of the shell.
    let edge: Vec<Vec3> = (0..body.verts.len())
        .filter(|&i| w[i] > 0.02 && w[i] < 0.35)
        .map(|i| b.p(i as u32))
        .collect();
    let from_edge = |p: Vec3| edge.iter().map(|e| e.distance(p)).fold(1.0f32, f32::min);
    let mut cap_map: HashMap<u32, u32> = HashMap::new();
    for t in body.indices.as_chunks::<3>().0 {
        if t.iter().any(|&i| w[i as usize] <= 0.02) {
            continue;
        }
        for &i in t {
            let id = *cap_map.entry(i).or_insert_with(|| {
                let p = Vec3::from(body.verts[i as usize]);
                let n = b.nrm[i as usize];
                let d = p - centre;
                let u = (d.x.atan2(d.z) / std::f32::consts::TAU + 0.5).clamp(0.0, 1.0);
                let t = if shell > 0.0 {
                    let x = (from_edge(p) / (shell * 1.6)).clamp(0.0, 1.0);
                    x * x * (3.0 - 2.0 * x)
                } else {
                    0.0
                };
                cards
                    .v
                    .push(p + n * (lift + shell * t * (1.0 + 0.25 * lump(p))));
                cards.n.push(n);
                cards.uv.push([u, 1.0 - w[i as usize]]);
                cards.anchor.push(i);
                (cards.v.len() - 1) as u32
            });
            cards.idx.push(id);
        }
    }
    let cap_tris = cards.idx.len();
    let mut strands = Cards::default();
    // ---- roots: area-weighted samples on the scalp (weight > 0.6).
    let tris: Vec<[u32; 3]> = body
        .indices
        .as_chunks::<3>()
        .0
        .iter()
        .copied()
        .filter(|t| t.iter().all(|&i| w[i as usize] > 0.6))
        .collect();
    let area: Vec<f32> = tris
        .iter()
        .map(|t| {
            let p = t.map(|i| b.p(i));
            (p[1] - p[0]).cross(p[2] - p[0]).length() * 0.5
        })
        .collect();
    let total: f32 = area.iter().sum();
    let mut want = hair.cards.unwrap_or(c.cards) as usize;
    let per_card_tris = c.segs.max(1) * 2;
    if let Some(t) = hair.tris {
        let left = (t as usize).saturating_sub(cap_tris / 3);
        want = want.min(left / per_card_tris.max(1));
    }
    let part_x = match part_side.as_str() {
        "left" => centre.x + 0.035,
        "right" => centre.x - 0.035,
        "middle" => centre.x,
        _ => f32::NAN,
    };
    let front_z = hi.z - 0.03;
    let lmp = |n: &str| lm.point(n).map(|v| cage[v as usize]);
    let brow_y = lmp("brow_peak_l").map_or(centre.y - 0.02, |p| p.y);
    let face = FaceBox {
        half_x: lmp("eye_corner_out_l").map_or(0.06, |p| p.x + 0.022),
        top_y: brow_y + 0.045,
        brow_y,
        bottom_y: lmp("chin").map_or(centre.y - 0.2, |p| p.y - 0.03),
        z0: lmp("ear_top_l").map_or(centre.z, |p| p.z + 0.035),
    };
    let ponytail_at = centre + Vec3::new(0.0, -0.01, -(hi.z - lo.z) * 0.5 - 0.01);
    if total > 0.0 && c.segs > 0 {
        let mut cum = Vec::with_capacity(area.len());
        let mut s = 0.0;
        for a in &area {
            s += a;
            cum.push(s);
        }
        for card in 0..want {
            let r = rng.f() * total;
            let ti = cum.partition_point(|x| *x < r).min(tris.len() - 1);
            let t = tris[ti];
            let (mut u, mut v) = (rng.f(), rng.f());
            if u + v > 1.0 {
                u = 1.0 - u;
                v = 1.0 - v;
            }
            let p = t.map(|i| b.p(i));
            let root = p[0] + (p[1] - p[0]) * u + (p[2] - p[0]) * v;
            let anchor = t[0];
            let n = (b.nrm[t[0] as usize] + b.nrm[t[1] as usize] + b.nrm[t[2] as usize])
                .normalize_or(Vec3::Y);
            let layer = card as f32 / want.max(1) as f32; // later cards on top
            let off = 0.002 + volume * 0.02 * (0.3 + 0.7 * layer) + 0.004 * rng.f();
            let pts = groom(
                b,
                root,
                n,
                centre,
                style,
                &c,
                length * (0.8 + 0.4 * rng.f()),
                off,
                part_x,
                fringe,
                front_z,
                ponytail_at,
                &face,
                &mut rng,
            );
            let m = pts.len();
            let mut side = Vec::with_capacity(m);
            let mut nrm = Vec::with_capacity(m);
            for i in 0..m {
                let d =
                    (pts[(i + 1).min(m - 1)] - pts[i.saturating_sub(1)]).normalize_or(Vec3::NEG_Y);
                let out = (pts[i] - centre).normalize_or(n);
                let sd = d.cross(out).normalize_or(Vec3::X);
                let tt = i as f32 / (m - 1).max(1) as f32;
                let wdt = c.width * (0.6 + 0.6 * rng.f().min(1.0)) * (1.0 - 0.55 * tt);
                side.push(sd * wdt * 0.5);
                nrm.push(sd.cross(d).normalize_or(out));
            }
            strands.strip(&pts, &side, &nrm, &[anchor], 0.0, 1.0);
        }
        // A ponytail / bun: the gathered mass.
        if style == "ponytail" {
            ponytail(&mut strands, b, ponytail_at, length, c.width, &mut rng);
        }
    }
    // Volumes: curly cards over the afro / curly mass, and a bun.
    if matches!(style, "afro" | "curly") {
        let surf: Vec<(Vec3, Vec3, u32)> = (0..cards.v.len())
            .filter(|&i| cards.uv[i][1] < 0.4)
            .map(|i| (cards.v[i], cards.n[i], cards.anchor[i]))
            .collect();
        let count = if style == "afro" { 1500 } else { 800 };
        curl_cards(&mut strands, &surf, count, &mut rng);
    }
    if style == "bun" {
        let cen = ponytail_at + Vec3::new(0.0, 0.045, 0.012);
        let anchor = b.nearest(cen);
        let base = cards.v.len() as u32;
        let (rings, segs) = (12u32, 18u32);
        let mut surf = Vec::new();
        for r in 0..=rings {
            let th = std::f32::consts::PI * r as f32 / rings as f32;
            for k in 0..=segs {
                let ph = std::f32::consts::TAU * k as f32 / segs as f32;
                let d = Vec3::new(th.sin() * ph.cos(), th.cos(), th.sin() * ph.sin());
                let p = cen + d * Vec3::new(0.05, 0.04, 0.045) * (1.0 + 0.08 * lump(d));
                cards.v.push(p);
                cards.n.push(d);
                cards.uv.push([k as f32 / segs as f32, 0.0]);
                cards.anchor.push(anchor);
                surf.push((p, d, anchor));
            }
        }
        for r in 0..rings {
            for k in 0..segs {
                let a0 = base + r * (segs + 1) + k;
                let (b0, c0, d0) = (a0 + 1, a0 + segs + 1, a0 + segs + 2);
                cards.idx.extend([a0, b0, c0, b0, d0, c0]);
            }
        }
        curl_cards(&mut strands, &surf, 180, &mut rng);
    }
    // Merge cap and strands into one part (two uv uses, one material
    // each): the cap first. Both take the strand material's cutout; the cap
    // gets its own look via a second part would cost a draw call, so the
    // cap is its own part only when there are no strands.
    let _ = short;
    if strands.v.is_empty() {
        return Ok(vec![cards.into_part(
            &format!("{name}_hair"),
            body,
            cap_graph(col, true),
            "human_hair_cap",
        )]);
    }
    Ok(vec![
        strands.into_part(
            &format!("{name}_hair"),
            body,
            strand_graph(col, 60.0, 0.85, 5),
            "human_hair",
        ),
        cards.into_part(
            &format!("{name}_hair_cap"),
            body,
            cap_graph(col, false),
            "human_hair_cap",
        ),
    ])
}

/// The centreline of one strand card.
#[allow(clippy::too_many_arguments)]
fn groom(
    b: &Body,
    root: Vec3,
    n: Vec3,
    centre: Vec3,
    style: &str,
    c: &StyleCfg,
    length: f32,
    off: f32,
    part_x: f32,
    fringe: f32,
    front_z: f32,
    tail_at: Vec3,
    face: &FaceBox,
    rng: &mut Rng,
) -> Vec<Vec3> {
    let segs = c.segs.max(1);
    let ds = length / segs as f32;
    // Groom direction in the scalp's tangent plane.
    let mut want = Vec3::new(0.0, -0.3, -1.0);
    if part_x.is_finite() {
        let s = (root.x - part_x).signum();
        // Away from the part on top and in front; straight down behind.
        let fz = ((root.z - centre.z) / 0.08 + 0.6).clamp(0.0, 1.0);
        want = Vec3::new(
            s * 1.2 * fz + (root.x - centre.x) * 8.0 * (1.0 - fz),
            -0.4,
            -0.35 - 0.8 * (1.0 - fz),
        );
    }
    let mut fringe_strand = false;
    if root.z > front_z && fringe > 0.0 && rng.f() < fringe + 0.1 {
        fringe_strand = true;
        // Fringe: forward and down over the forehead.
        want = Vec3::new((root.x - centre.x) * 3.0, -0.6, 1.0);
    }
    if matches!(style, "ponytail" | "bun") {
        want = tail_at - root;
    }
    let tangent = |d: Vec3, n: Vec3| (d - n * d.dot(n)).normalize_or(Vec3::NEG_Z);
    let mut d = (tangent(want, n) + n * 0.15).normalize();
    let mut p = root + n * off;
    let mut pts = vec![root + n * (off * 0.3)];
    let phase = rng.f() * std::f32::consts::TAU;
    let wave = 0.025 + 0.02 * rng.f();
    for k in 1..=segs {
        let s = k as f32 / segs as f32;
        // Gravity grows along the strand (stiff near the root).
        // By arc length: full weight after ~6 cm, whatever the length.
        let reach = (s * length / 0.06).min(1.0);
        let g = c.gravity * reach * reach * 1.5;
        let to_tail = if matches!(style, "ponytail" | "bun") {
            (tail_at - p).normalize_or(Vec3::NEG_Y) * 1.2
        } else {
            Vec3::ZERO
        };
        d = (d + Vec3::NEG_Y * g + to_tail).normalize_or(Vec3::NEG_Y);
        let mut q = p + d * ds;
        if c.curl > 0.0 {
            let ang = phase + std::f32::consts::TAU * (s * length) / wave;
            let out = (q - centre).normalize_or(n);
            let sd = d.cross(out).normalize_or(Vec3::X);
            q += (sd * ang.cos() + out * ang.sin()) * (c.curl * 0.006 * s.sqrt());
        }
        // Stay off the body (head, neck, shoulders, back) and out of the
        // face: a fringe stops above the brows, other strands go round it.
        let clear = off.max(0.003);
        q = b.push_out(q, clear);
        // Over the head the strand follows the scalp at its own layer
        // height; below the jaw it hangs free.
        if q.y > face.bottom_y + 0.05 {
            let i = b.nearest(q);
            let (v, nv) = (b.p(i), b.nrm[i as usize]);
            let h = (q - v).dot(nv);
            if h > clear && (q - v).length() < 0.05 {
                q -= nv * ((h - clear) * 0.85);
            }
        }
        let mut steer = None;
        if face.inside(q) {
            if fringe_strand {
                if q.y < face.brow_y + 0.008 {
                    break;
                }
            } else {
                // Round the face: drift out to its side, then fall.
                let sx = if q.x >= 0.0 { 1.0 } else { -1.0 };
                let gap = face.half_x - q.x.abs();
                q.x += sx * gap.min(ds * 0.8);
                q = b.push_out(q, clear);
                steer = Some(Vec3::new(sx * 0.5, -1.0, -0.3).normalize());
            }
        }
        d = steer.unwrap_or_else(|| (q - p).normalize_or(d));
        p = q;
        pts.push(p);
    }
    pts
}

/// A ponytail: strands hanging from the gather point, with a tie.
fn ponytail(cards: &mut Cards, b: &Body, at: Vec3, length: f32, width: f32, rng: &mut Rng) {
    let anchor = b.nearest(at);
    for _ in 0..90 {
        let a = rng.f() * std::f32::consts::TAU;
        let rr = 0.012 * rng.f().sqrt();
        let root = at + Vec3::new(a.cos() * rr, a.sin() * rr, -0.005);
        let mut pts = vec![root];
        let mut d = Vec3::new(a.cos() * 0.2, -0.7, -0.6).normalize();
        let segs = 8;
        let l = length * (0.75 + 0.35 * rng.f());
        for k in 1..=segs {
            let s = k as f32 / segs as f32;
            d = (d + Vec3::NEG_Y * (0.6 + s)).normalize();
            let mut q = *pts.last().unwrap_or(&root) + d * (l / segs as f32);
            // A slight spread mid-tail, gathering again at the tips.
            q += Vec3::new(a.cos(), 0.0, a.sin()) * (0.004 * (std::f32::consts::PI * s).sin());
            q = b.push_out(q, 0.006);
            pts.push(q);
        }
        let m = pts.len();
        let mut side = Vec::new();
        let mut nrm = Vec::new();
        for i in 0..m {
            let dd = (pts[(i + 1).min(m - 1)] - pts[i.saturating_sub(1)]).normalize_or(Vec3::NEG_Y);
            let sd = dd
                .cross(Vec3::new(a.cos(), 0.0, a.sin()))
                .normalize_or(Vec3::X);
            let tt = i as f32 / (m - 1) as f32;
            side.push(sd * width * 0.5 * (1.0 - 0.5 * tt));
            nrm.push(sd.cross(dd).normalize_or(Vec3::Z));
        }
        cards.strip(&pts, &side, &nrm, &[anchor], 0.0, 1.0);
    }
}

/// Cards tangent to a noisy ellipsoid (afro / curly volume / bun): short
/// Short curved cards lying on a surface (afro, curls, bun): each
/// sampled at a surface point, curling round its normal.
fn curl_cards(cards: &mut Cards, surf: &[(Vec3, Vec3, u32)], count: usize, rng: &mut Rng) {
    if surf.is_empty() {
        return;
    }
    for _ in 0..count {
        let (p, n, anchor) = surf[((rng.f() * surf.len() as f32) as usize).min(surf.len() - 1)];
        let t1 = n.any_orthonormal_vector();
        let t2 = n.cross(t1);
        let ang = rng.f() * std::f32::consts::TAU;
        let along = t1 * ang.cos() + t2 * ang.sin();
        let l = 0.025 + 0.02 * rng.f();
        let bend = 1.5 + 2.0 * rng.f();
        let pts: Vec<Vec3> = (0..5)
            .map(|k| {
                let s = k as f32 / 4.0;
                let a = bend * (s - 0.3);
                let side = n.cross(along);
                p + n * (0.002 + 0.006 * s)
                    + (along * a.cos() + side * a.sin() - along) * (l / bend)
                    + along * (s * l * 0.3)
            })
            .collect();
        let m = pts.len();
        let mut side = Vec::with_capacity(m);
        let mut nrm = Vec::with_capacity(m);
        for i in 0..m {
            let d = (pts[(i + 1).min(m - 1)] - pts[i.saturating_sub(1)]).normalize_or(along);
            side.push(d.cross(n).normalize_or(t1) * 0.004);
            nrm.push(n);
        }
        cards.strip(&pts, &side, &nrm, &[anchor], 0.0, 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn human(extra: serde_json::Value) -> crate::body::BodyDef {
        let mut v = json!({"preset": "human"});
        if let (Some(m), Some(e)) = (v.as_object_mut(), extra.as_object()) {
            m.extend(e.clone());
        }
        serde_json::from_value(v).unwrap_or_else(|e| panic!("{e}"))
    }

    fn part<'a>(ps: &'a [Part], n: &str) -> &'a Part {
        ps.iter()
            .find(|p| p.name == n)
            .unwrap_or_else(|| panic!("no part {n}"))
    }

    #[test]
    fn every_style_builds_with_brows_and_lashes() {
        if !super::super::asset::available() {
            return;
        }
        for s in STYLES {
            let d = human(json!({"hair": {"style": s}}));
            d.validate().unwrap_or_else(|e| panic!("{s}: {e}"));
            let t = std::time::Instant::now();
            let ps = super::super::build("eve", &d).unwrap_or_else(|e| panic!("{s}: {e}"));
            let names: Vec<&str> = ps.iter().map(|p| p.name.as_str()).collect();
            assert!(
                names.contains(&"eve_brows") && names.contains(&"eve_lashes"),
                "{names:?}"
            );
            assert_eq!(names.contains(&"eve_hair"), *s != "bald", "{s}");
            if *s != "bald" {
                let h = part(&ps, "eve_hair");
                assert!(
                    h.cards && h.tri_count() > 100,
                    "{s}: {} tris",
                    h.tri_count()
                );
                assert!(h.skin.is_some());
                assert!(h.verts.iter().all(|v| v.iter().all(|x| x.is_finite())));
                println!("{s}: {} tris, {:?}", h.tri_count(), t.elapsed());
            }
        }
    }

    #[test]
    fn lashes_follow_a_blink_and_brows_a_raise() {
        if !super::super::asset::available() {
            return;
        }
        let open = super::super::build("eve", &human(json!({"hair": "bald"})))
            .unwrap_or_else(|e| panic!("{e}"));
        let shut = super::super::build(
            "eve",
            &human(json!({"hair": "bald", "expression": {"eye-left-closure": 1.0, "eye-right-closure": 1.0,
                "eyebrows-left-up": 1.0, "eyebrows-right-up": 1.0}})),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let top = |ps: &[Part], n: &str| {
            let p = part(ps, n);
            p.verts.iter().map(|v| v[1]).sum::<f32>() / p.verts.len() as f32
        };
        let dl = top(&open, "eve_lashes") - top(&shut, "eve_lashes");
        assert!(dl > 0.002, "lashes dropped {dl}");
        let db = top(&shut, "eve_brows") - top(&open, "eve_brows");
        assert!(db > 0.001, "brows rose {db}");
    }
}

#[cfg(test)]
mod bake_check {
    #[test]
    fn card_materials_have_cutouts() {
        for (n, g) in [
            (
                "strand",
                super::strand_graph(super::DEFAULT_COLOR, 60.0, 0.85, 5),
            ),
            ("cap", super::cap_graph(super::DEFAULT_COLOR, true)),
            ("brow", super::brow_graph(super::DEFAULT_COLOR)),
            ("lash", super::lash_graph([0.05, 0.04, 0.03])),
        ] {
            let s = crate::material::Samples::tile(128, 1.0);
            let c = crate::material::bake(&g.nodes, &g.params, &s, false)
                .unwrap_or_else(|e| panic!("{n}: {e}"));
            let on = c.opacity.iter().filter(|x| **x > 0.5).count() as f32 / c.opacity.len() as f32;
            let m = c.base_color.iter().map(|b| b[0]).sum::<f32>() / c.base_color.len() as f32;
            println!("{n}: coverage {on:.2} mean r {m:.3}");
            assert!(on > 0.05 && on < 0.95, "{n}: coverage {on}");
        }
    }
}
