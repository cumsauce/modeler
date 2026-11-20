//! The `human` body preset: a parametric template instead of a field.
//!
//! The template is MakeHuman's CC0 base mesh (a closed quad skin plus eye,
//! teeth and tongue helpers) with its morph targets, skeleton and skin
//! weights, packed into `data/human/human.bin` (see [`asset`]). A human
//! is the base mesh plus a weighted sum of targets:
//!
//! - macro targets, blended multilinearly between the nodes the files are
//!   named after: gender (female 0 .. male 1), age (baby 1 y, child 11 y,
//!   young 25 y, old 90 y), muscle and weight (min, average, max at 0, 0.5,
//!   1), plus ethnicity (`{african, asian, caucasian}-{gender}-{age}`),
//!   height and proportions (`-min|maxheight`, `-uncommon|idealproportions`
//!   on top of the same four) and the breast cup / firmness set;
//! - detail keys (`shape`: `nose-width1` -1..1 between its `decr` / `incr`
//!   files), expression units (`expression`: 0..1);
//!
//! then scaled uniformly to `height` and grounded. The topology never
//! changes, so local warps are vertex displacements, `detail` levels are
//! Catmull-Clark over the quad cage (weights, morph deltas and uvs carried
//! through by the same stencils) and `shapes` export as glTF morph targets.

pub mod asset;

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use bevy::math::Vec3;
use serde::{Deserialize, Serialize};

use crate::Part;
use asset::HumanAsset;

/// Extra parts a human can make (`<name>_eyes` ...).
pub const EXTRAS: &[&str] = &["eyes", "teeth", "tongue"];

/// A named morph target of a human item: its own expression / shape values
/// on top of the item's.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ShapeKey {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub expression: BTreeMap<String, f32>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub shape: BTreeMap<String, f32>,
}

/// Skin weights that come with a mesh (the template's): per vertex up to
/// four (bone, weight) pairs, bones by name.
#[derive(Clone, Debug, PartialEq)]
pub struct SkinWeights {
    pub bones: Arc<Vec<String>>,
    pub joints: Vec<[u16; 4]>,
    pub weights: Vec<[f32; 4]>,
}

/// Age in years -> MakeHuman's 0..1 scale (baby 1 y = 0, child 11 y =
/// 0.1875, young 25 y = 0.5, old 90 y = 1). Values <= 1 are already 0..1.
pub fn age_unit(a: f32) -> f32 {
    if a <= 1.0 {
        return a.clamp(0.0, 1.0);
    }
    let pts = [(1.0, 0.0), (11.0, 0.1875), (25.0, 0.5), (90.0, 1.0)];
    for w in pts.windows(2) {
        let ((y0, u0), (y1, u1)) = (w[0], w[1]);
        if a <= y1 {
            return u0 + (u1 - u0) * (a - y0) / (y1 - y0);
        }
    }
    1.0
}

/// The resolved parameters of a human.
#[derive(Clone, Debug, PartialEq)]
pub struct Params {
    pub gender: f32,
    pub age: f32,
    pub muscle: f32,
    pub weight: f32,
    pub proportions: f32,
    pub ethnicity: [f32; 3],
    pub breast_size: f32,
    pub breast_firmness: f32,
    pub shape: BTreeMap<String, f32>,
    pub expression: BTreeMap<String, f32>,
}

impl Params {
    pub fn of(def: &crate::body::BodyDef) -> Result<Self, String> {
        let mut eth = [1.0f32 / 3.0; 3];
        if let Some(e) = &def.ethnicity {
            eth = [0.0; 3];
            for (k, v) in e {
                let i = match k.as_str() {
                    "african" => 0,
                    "asian" => 1,
                    "caucasian" => 2,
                    o => {
                        return Err(format!(
                            "ethnicity: `{o}` is not african, asian or caucasian"
                        ));
                    }
                };
                if !v.is_finite() || *v < 0.0 {
                    return Err(format!("ethnicity.{k}: must be >= 0"));
                }
                eth[i] = *v;
            }
            let s: f32 = eth.iter().sum();
            if s <= 0.0 {
                return Err("ethnicity: the weights sum to 0".into());
            }
            eth = eth.map(|x| x / s);
        }
        let unit = |f: &str, v: Option<f32>, d: f32| -> Result<f32, String> {
            match v {
                Some(x) if !x.is_finite() || !(0.0..=1.0).contains(&x) => {
                    Err(format!("{f}: must be in 0..1, got {x}"))
                }
                Some(x) => Ok(x),
                None => Ok(d),
            }
        };
        if let Some(a) = def.age
            && (!a.is_finite() || !(0.0..=120.0).contains(&a))
        {
            return Err(format!(
                "age: years (1..120) or 0..1 (MakeHuman's scale), got {a}"
            ));
        }
        Ok(Params {
            gender: unit("gender", def.gender, 0.5)?,
            age: age_unit(def.age.unwrap_or(25.0)),
            muscle: unit("muscle", def.muscle, 0.5)?,
            weight: unit("weight", def.weight, 0.5)?,
            proportions: unit("proportions", def.proportions, 0.5)?,
            ethnicity: eth,
            breast_size: unit("breast_size", def.breast_size, 0.5)?,
            breast_firmness: unit("breast_firmness", def.breast_firmness, 0.5)?,
            shape: def.shape.clone(),
            expression: def.expression.clone(),
        })
    }
}

/// Linear weights between the nodes at `xs` (sorted), named `names`.
fn nodes<'a>(x: f32, xs: &[f32], names: &[&'a str]) -> Vec<(&'a str, f32)> {
    if x <= xs[0] {
        return vec![(names[0], 1.0)];
    }
    for i in 1..xs.len() {
        if x <= xs[i] {
            let t = (x - xs[i - 1]) / (xs[i] - xs[i - 1]);
            return [(names[i - 1], 1.0 - t), (names[i], t)]
                .into_iter()
                .filter(|w| w.1 > 1e-6)
                .collect();
        }
    }
    vec![(names[names.len() - 1], 1.0)]
}

/// The macro targets and their weights for these parameters (height
/// targets apart, for the stature search).
fn macro_weights(p: &Params) -> (Vec<(String, f32)>, Vec<(String, f32)>, Vec<(String, f32)>) {
    let g = nodes(p.gender, &[0.0, 1.0], &["female", "male"]);
    let a = nodes(
        p.age,
        &[0.0, 0.1875, 0.5, 1.0],
        &["baby", "child", "young", "old"],
    );
    let m = nodes(p.muscle, &[0.0, 0.5, 1.0], &["min", "average", "max"]);
    let w = nodes(p.weight, &[0.0, 0.5, 1.0], &["min", "average", "max"]);
    let mut uni = Vec::new();
    let mut hmax = Vec::new();
    let mut hmin = Vec::new();
    for (gn, gw) in &g {
        for (an, aw) in &a {
            for (k, e) in ["african", "asian", "caucasian"].iter().enumerate() {
                let x = gw * aw * p.ethnicity[k];
                if x > 1e-6 {
                    uni.push((format!("macrodetails/{e}-{gn}-{an}"), x));
                }
            }
            for (mn, mw) in &m {
                for (wn, ww) in &w {
                    let x = gw * aw * mw * ww;
                    if x <= 1e-6 {
                        continue;
                    }
                    let stem = format!("{gn}-{an}-{mn}muscle-{wn}weight");
                    uni.push((format!("macrodetails/universal-{stem}"), x));
                    hmax.push((format!("macrodetails/height/{stem}-maxheight"), x));
                    hmin.push((format!("macrodetails/height/{stem}-minheight"), x));
                    let pr = (p.proportions - 0.5) * 2.0;
                    if pr.abs() > 1e-6 && *an != "baby" {
                        let side = if pr > 0.0 { "ideal" } else { "uncommon" };
                        uni.push((
                            format!("macrodetails/proportions/{stem}-{side}proportions"),
                            x * pr.abs(),
                        ));
                    }
                    if *gn == "female" {
                        let cup =
                            nodes(p.breast_size, &[0.0, 0.5, 1.0], &["min", "average", "max"]);
                        let firm = nodes(p.breast_firmness, &[0.0, 1.0], &["min", "max"]);
                        for (cn, cw) in &cup {
                            for (fn_, fw) in &firm {
                                uni.push((
                                    format!("breast/{stem}-{cn}cup-{fn_}firmness"),
                                    x * cw * fw,
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
    (uni, hmax, hmin)
}

fn add_target(acc: &mut [Vec3], a: &HumanAsset, i: usize, w: f32) {
    let t = &a.targets[i];
    let k = w * asset::QUANTUM;
    for (&v, d) in t.idx.iter().zip(&t.d) {
        acc[v as usize] += Vec3::new(d[0] as f32, d[1] as f32, d[2] as f32) * k;
    }
}

fn add_named(acc: &mut [Vec3], a: &HumanAsset, list: &[(String, f32)]) {
    for (n, w) in list {
        // Missing macro files (a node the set doesn't have) contribute
        // nothing, like a zero target.
        if let Some(i) = a.target_index(n) {
            add_target(acc, a, i, *w);
        }
    }
}

/// Close names for an error.
fn close<'a>(name: &str, all: impl Iterator<Item = &'a str>) -> String {
    let mut v: Vec<(usize, &str)> = all
        .map(|n| (crate::spec::edit_distance(name, n), n))
        .collect();
    v.sort();
    let near: Vec<&str> = v.iter().take(5).map(|x| x.1).collect();
    near.join(", ")
}

/// Offsets of detail keys and expression units (in template meters).
fn add_details(
    acc: &mut [Vec3],
    a: &HumanAsset,
    shape: &BTreeMap<String, f32>,
    expression: &BTreeMap<String, f32>,
    eth: [f32; 3],
) -> Result<(), String> {
    for (k, &v) in shape {
        let Some(key) = a.keys.iter().find(|x| &x.name == k) else {
            return Err(format!(
                "shape: no detail target `{k}` (close: {})",
                close(k, a.keys.iter().map(|x| x.name.as_str()))
            ));
        };
        if !v.is_finite() || !(-1.0..=1.0).contains(&v) {
            return Err(format!("shape.{k}: must be in -1..1, got {v}"));
        }
        if v < 0.0 {
            if key.low < 0 {
                return Err(format!("shape.{k}: one-sided, 0..1 only"));
            }
            add_target(acc, a, key.low as usize, -v);
        } else if v > 0.0 && key.high >= 0 {
            add_target(acc, a, key.high as usize, v);
        }
    }
    if expression.is_empty() {
        return Ok(());
    }
    let mut ex = vec![Vec3::ZERO; acc.len()];
    for (k, &v) in expression {
        let Some(u) = a.units.iter().find(|x| &x.name == k) else {
            return Err(format!(
                "expression: no unit `{k}` (close: {})",
                close(k, a.units.iter().map(|x| x.name.as_str()))
            ));
        };
        if !v.is_finite() || !(0.0..=1.0).contains(&v) {
            return Err(format!("expression.{k}: must be in 0..1, got {v}"));
        }
        for (e, &t) in u.by_ethnicity.iter().enumerate() {
            if t >= 0 && eth[e] > 0.0 {
                add_target(&mut ex, a, t as usize, v * eth[e]);
            }
        }
    }
    tongue_follows(a, &mut ex);
    for (x, d) in acc.iter_mut().zip(ex) {
        *x += d;
    }
    Ok(())
}

/// The measurements `measure` takes: (name, MakeHuman measure key, limb
/// side only (x > 0), ring axis: None = up, Some((from, to)) = along the
/// segment between those joint refs).
pub const MEASURES: &[(&str, &str, bool, Option<(&str, &str)>)] = &[
    (
        "neck",
        "measure-neck-circ",
        false,
        Some(("neck01____head", "head____head")),
    ),
    ("chest", "measure-bust-circ", false, None),
    ("underbust", "measure-underbust-circ", false, None),
    ("waist", "measure-waist-circ", false, None),
    ("hips", "measure-hips-circ", false, None),
    (
        "upper_arm",
        "measure-upperarm-circ",
        true,
        Some(("upperarm01.L____head", "lowerarm01.L____head")),
    ),
    (
        "wrist",
        "measure-wrist-circ",
        true,
        Some(("lowerarm01.L____head", "wrist.L____head")),
    ),
    (
        "thigh",
        "measure-thigh-circ",
        true,
        Some(("upperleg01.L____head", "lowerleg01.L____head")),
    ),
    (
        "knee",
        "measure-knee-circ",
        true,
        Some(("upperleg01.L____head", "lowerleg01.L____head")),
    ),
    (
        "calf",
        "measure-calf-circ",
        true,
        Some(("lowerleg01.L____head", "foot.L____head")),
    ),
    (
        "ankle",
        "measure-ankle-circ",
        true,
        Some(("lowerleg01.L____head", "foot.L____head")),
    ),
];

/// Where a measure's tape goes: the centre (the target's most-moved
/// vertices, weighted) and the ring's normal, in template space.
fn tape(
    a: &HumanAsset,
    raw: &[Vec3],
    m: &(&str, &str, bool, Option<(&str, &str)>),
) -> Option<(Vec3, Vec3)> {
    let key = a.keys.iter().find(|k| k.name == m.1)?;
    let t = a.targets.get(key.high.max(key.low) as usize)?;
    let mags: Vec<(u16, f32)> = t
        .idx
        .iter()
        .zip(&t.d)
        .filter(|(v, _)| !m.2 || raw[**v as usize].x > 0.0)
        .map(|(v, d)| {
            (
                *v,
                Vec3::new(d[0] as f32, d[1] as f32, d[2] as f32).length(),
            )
        })
        .collect();
    let top = mags.iter().map(|x| x.1).fold(0.0f32, f32::max);
    if top <= 0.0 {
        return None;
    }
    let (mut c, mut w) = (Vec3::ZERO, 0.0);
    for &(v, g) in &mags {
        if g > 0.7 * top {
            c += raw[v as usize] * g;
            w += g;
        }
    }
    let c = c / w.max(1e-9);
    let n = match m.3 {
        None => Vec3::Y,
        Some((f, t)) => {
            let jp = |n: &str| {
                let j = a.joints.iter().find(|j| j.0 == n)?;
                Some(j.1.iter().map(|&v| raw[v as usize]).sum::<Vec3>() / j.1.len().max(1) as f32)
            };
            (jp(t)? - jp(f)?).try_normalize()?
        }
    };
    Some((c, n))
}

/// A tape measure round the body: the convex perimeter of where the plane
/// (`c`, `n`) cuts the skin within `reach` of `c` (meters, in the space of
/// `pos`).
pub fn girth(
    a: &HumanAsset,
    pos: &[Vec3],
    c: Vec3,
    n: Vec3,
    reach: f32,
    only: Option<&[bool]>,
) -> f32 {
    let u = n.any_orthonormal_vector();
    let v = n.cross(u);
    let mut pts: Vec<[f32; 2]> = Vec::new();
    for f in a.faces.iter().filter(|f| f.group == 0) {
        if only.is_some_and(|o| f.v.iter().any(|&v| !o[v as usize])) {
            continue;
        }
        for k in 0..4 {
            let (p, q) = (pos[f.v[k] as usize], pos[f.v[(k + 1) % 4] as usize]);
            let (dp, dq) = ((p - c).dot(n), (q - c).dot(n));
            if dp * dq > 0.0 || dp == dq {
                continue;
            }
            let x = p + (q - p) * (dp / (dp - dq));
            if x.distance(c) < reach {
                let r = x - c;
                pts.push([r.dot(u), r.dot(v)]);
            }
        }
    }
    hull_perimeter(&mut pts)
}

fn hull_perimeter(pts: &mut [[f32; 2]]) -> f32 {
    if pts.len() < 3 {
        return 0.0;
    }
    pts.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    let cross = |o: [f32; 2], a: [f32; 2], b: [f32; 2]| {
        (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
    };
    let mut h: Vec<[f32; 2]> = Vec::new();
    for pass in 0..2 {
        let start = h.len();
        let it: Box<dyn Iterator<Item = &[f32; 2]>> = if pass == 0 {
            Box::new(pts.iter())
        } else {
            Box::new(pts.iter().rev())
        };
        for &p in it {
            while h.len() >= start + 2 && cross(h[h.len() - 2], h[h.len() - 1], p) <= 0.0 {
                h.pop();
            }
            h.push(p);
        }
        h.pop();
    }
    (0..h.len())
        .map(|i| {
            let (a, b) = (h[i], h[(i + 1) % h.len()]);
            ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
        })
        .sum()
}

/// The girth of measure `name` on the template positions `raw` at
/// `scale` (meters), or None for a name it doesn't know.
pub fn measured(a: &HumanAsset, raw: &[Vec3], scale: f32, name: &str) -> Option<f32> {
    let m = MEASURES.iter().find(|m| m.0 == name)?;
    let (c, n) = tape(a, raw, m)?;
    let reach = if m.2 || name == "neck" { 0.12 } else { 0.3 } / scale.max(1e-6);
    // Only the skin the measure's own target shapes (the torso targets
    // leave the arms alone, so the tape doesn't wrap them too).
    let key = a.keys.iter().find(|k| k.name == m.1)?;
    let mut only = vec![false; raw.len()];
    for t in [key.low, key.high].into_iter().filter(|&t| t >= 0) {
        for &v in &a.targets.get(t as usize)?.idx {
            only[v as usize] = true;
        }
    }
    Some(girth(a, raw, c, n, reach, Some(&only)) * scale)
}

/// Solves each measure's slider (-1..1, a 1D bisection on the measured
/// girth; two rounds, since the targets overlap a little).
fn solve_measures(
    a: &HumanAsset,
    raw: &mut [Vec3],
    scale: f32,
    want: &BTreeMap<String, f32>,
) -> Result<(), String> {
    for (k, &g) in want {
        if !MEASURES.iter().any(|m| m.0 == k) {
            return Err(format!(
                "measure: no `{k}` (one of: {})",
                MEASURES.iter().map(|m| m.0).collect::<Vec<_>>().join(", ")
            ));
        }
        if !g.is_finite() || !(0.05..=3.0).contains(&g) {
            return Err(format!("measure.{k}: a girth in meters (0.05..3), got {g}"));
        }
    }
    let mut applied: BTreeMap<String, f32> = BTreeMap::new();
    for _round in 0..2 {
        for (k, &g) in want {
            let Some(m) = MEASURES.iter().find(|m| m.0 == k) else {
                continue;
            };
            let Some(key) = a.keys.iter().find(|x| x.name == m.1) else {
                return Err(format!("measure.{k}: the template has no `{}`", m.1));
            };
            // Undo this measure's current slider, then search it afresh.
            let cur = applied.get(k).copied().unwrap_or(0.0);
            let mut base: Vec<Vec3> = raw.to_vec();
            let mut d_lo = vec![Vec3::ZERO; raw.len()];
            let mut d_hi = vec![Vec3::ZERO; raw.len()];
            if key.low >= 0 {
                add_target(&mut d_lo, a, key.low as usize, 1.0);
            }
            if key.high >= 0 {
                add_target(&mut d_hi, a, key.high as usize, 1.0);
            }
            let shift = |b: &[Vec3], t: f32| -> Vec<Vec3> {
                b.iter()
                    .enumerate()
                    .map(|(i, p)| *p + if t >= 0.0 { d_hi[i] * t } else { d_lo[i] * -t })
                    .collect()
            };
            base = shift(&base, -cur);
            let f = |t: f32| measured(a, &shift(&base, t), scale, k).unwrap_or(0.0);
            let (mut lo, mut hi) = (-1.0f32, 1.0f32);
            let (glo, ghi) = (f(lo), f(hi));
            let t = if g <= glo.min(ghi) {
                if glo < ghi { lo } else { hi }
            } else if g >= glo.max(ghi) {
                if glo > ghi { lo } else { hi }
            } else {
                let up = ghi > glo;
                for _ in 0..22 {
                    let mid = 0.5 * (lo + hi);
                    if (f(mid) < g) == up {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                0.5 * (lo + hi)
            };
            let moved = shift(&base, t);
            raw.copy_from_slice(&moved);
            applied.insert(k.clone(), t);
        }
    }
    Ok(())
}

/// The evaluated template: morphed vertex positions in meters, grounded,
/// at the item's height; the transform from template space.
pub struct Eval {
    pub pos: Vec<Vec3>,
    pub scale: f32,
    pub offset: Vec3,
    /// Template-space morph before scaling (shape keys diff against it).
    pub raw: Vec<Vec3>,
}

fn body_extent_y(a: &HumanAsset, pos: &[Vec3]) -> (f32, f32) {
    let mut lo = f32::MAX;
    let mut hi = f32::MIN;
    for f in a.faces.iter().filter(|f| f.group == 0) {
        for &v in &f.v {
            let y = pos[v as usize].y;
            lo = lo.min(y);
            hi = hi.max(y);
        }
    }
    (lo, hi)
}

type EvalCache = Vec<(u64, Arc<Eval>)>;
static EVALS: Mutex<EvalCache> = Mutex::new(Vec::new());

fn hash_of(v: &impl Serialize) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(v).unwrap_or_default().hash(&mut h);
    h.finish()
}

#[derive(Serialize)]
struct EvalKey<'a> {
    gender: Option<f32>,
    age: Option<f32>,
    muscle: Option<f32>,
    weight: Option<f32>,
    proportions: Option<f32>,
    height: Option<f32>,
    breast_size: Option<f32>,
    breast_firmness: Option<f32>,
    ethnicity: &'a Option<BTreeMap<String, f32>>,
    shape: &'a BTreeMap<String, f32>,
    expression: &'a BTreeMap<String, f32>,
    measure: &'a BTreeMap<String, f32>,
}

fn eval_key(def: &crate::body::BodyDef) -> u64 {
    hash_of(&EvalKey {
        gender: def.gender,
        age: def.age,
        muscle: def.muscle,
        weight: def.weight,
        proportions: def.proportions,
        height: def.height,
        breast_size: def.breast_size,
        breast_firmness: def.breast_firmness,
        ethnicity: &def.ethnicity,
        shape: &def.shape,
        expression: &def.expression,
        measure: &def.measure,
    })
}

/// Morphs the template for `def` (cached per parameter set).
pub fn evaluate(def: &crate::body::BodyDef) -> Result<Arc<Eval>, String> {
    let key = eval_key(def);
    if let Ok(c) = EVALS.lock()
        && let Some(e) = c.iter().find(|e| e.0 == key)
    {
        return Ok(e.1.clone());
    }
    let a = asset::load()?;
    let p = Params::of(def)?;
    let n = a.verts.len();
    let mut raw: Vec<Vec3> = a.verts.iter().map(|v| Vec3::from(*v)).collect();
    let (uni, hmax, hmin) = macro_weights(&p);
    add_named(&mut raw, &a, &uni);
    add_details(&mut raw, &a, &p.shape, &p.expression, p.ethnicity)?;
    // Height: MakeHuman's height targets keep their proportions (longer
    // legs, not a scaled child); search the slider that reaches the asked
    // stature, then scale uniformly to it exactly.
    let mut scale = 1.0;
    if let Some(h) = def.height {
        let mut up = vec![Vec3::ZERO; n];
        add_named(&mut up, &a, &hmax);
        let mut down = vec![Vec3::ZERO; n];
        add_named(&mut down, &a, &hmin);
        let stature = |t: f32| {
            // t in -1..1: below 0 the min-height set, above the max.
            let tmp: Vec<Vec3> = (0..n)
                .map(|i| raw[i] + if t >= 0.0 { up[i] * t } else { down[i] * -t })
                .collect();
            let (lo, hi) = body_extent_y(&a, &tmp);
            hi - lo
        };
        let (s0, s_lo, s_hi) = (stature(0.0), stature(-1.0), stature(1.0));
        let t = if h >= s0 {
            if s_hi > s0 {
                ((h - s0) / (s_hi - s0)).clamp(0.0, 1.0)
            } else {
                0.0
            }
        } else if s_lo < s0 {
            -((s0 - h) / (s0 - s_lo)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        for i in 0..n {
            raw[i] += if t >= 0.0 { up[i] * t } else { down[i] * -t };
        }
        let (lo, hi) = body_extent_y(&a, &raw);
        scale = h / (hi - lo).max(1e-6);
    }
    if !def.measure.is_empty() {
        solve_measures(&a, &mut raw, scale, &def.measure)?;
    }
    let (lo, _) = body_extent_y(&a, &raw);
    let offset = Vec3::new(0.0, -lo * scale, 0.0);
    let pos = raw.iter().map(|v| *v * scale + offset).collect();
    let e = Arc::new(Eval {
        pos,
        scale,
        offset,
        raw,
    });
    if let Ok(mut c) = EVALS.lock() {
        c.retain(|x| x.0 != key);
        c.push((key, e.clone()));
        if c.len() > 12 {
            c.remove(0);
        }
    }
    Ok(e)
}

// ------------------------------------------------------------------ rig

/// One bone of the rig made from the template: its name, parent (index
/// into the list), head and tail joint refs, and the template bones whose
/// weights it takes.
#[derive(Clone, Debug)]
pub struct RigBone {
    pub name: String,
    pub parent: Option<usize>,
    pub head: usize,
    pub tail: Option<usize>,
}

/// The game rig: (name, parent, head = `<mh bone>` head or `<mh bone>.tail`,
/// tail, template bones merged in besides the head's own).
const GAME: &[(&str, &str, &str, &str)] = &[
    ("hip", "", "root", "spine04"),
    ("spine", "hip", "spine04", "spine02"),
    ("chest", "spine", "spine02", "neck01"),
    ("neck", "chest", "neck01", "head"),
    ("head", "neck", "head", "head.tail"),
    ("jaw", "head", "jaw", "jaw.tail"),
    ("eye_S", "head", "eye.S", "eye.S.tail"),
    ("clavicle_S", "chest", "clavicle.S", "upperarm01.S"),
    ("shoulder_S", "clavicle_S", "upperarm01.S", "lowerarm01.S"),
    ("elbow_S", "shoulder_S", "lowerarm01.S", "wrist.S"),
    ("wrist_S", "elbow_S", "wrist.S", "metacarpal2.S.tail"),
    (
        "hand_S",
        "wrist_S",
        "metacarpal2.S.tail",
        "finger3-1.S.tail",
    ),
    ("thumb_1_S", "wrist_S", "finger1-1.S", "finger1-1.S.tail"),
    ("thumb_2_S", "thumb_1_S", "finger1-2.S", "finger1-2.S.tail"),
    ("thumb_3_S", "thumb_2_S", "finger1-3.S", "finger1-3.S.tail"),
    ("index_1_S", "hand_S", "finger2-1.S", "finger2-1.S.tail"),
    ("index_2_S", "index_1_S", "finger2-2.S", "finger2-2.S.tail"),
    ("index_3_S", "index_2_S", "finger2-3.S", "finger2-3.S.tail"),
    ("middle_1_S", "hand_S", "finger3-1.S", "finger3-1.S.tail"),
    (
        "middle_2_S",
        "middle_1_S",
        "finger3-2.S",
        "finger3-2.S.tail",
    ),
    (
        "middle_3_S",
        "middle_2_S",
        "finger3-3.S",
        "finger3-3.S.tail",
    ),
    ("ring_1_S", "hand_S", "finger4-1.S", "finger4-1.S.tail"),
    ("ring_2_S", "ring_1_S", "finger4-2.S", "finger4-2.S.tail"),
    ("ring_3_S", "ring_2_S", "finger4-3.S", "finger4-3.S.tail"),
    ("pinky_1_S", "hand_S", "finger5-1.S", "finger5-1.S.tail"),
    ("pinky_2_S", "pinky_1_S", "finger5-2.S", "finger5-2.S.tail"),
    ("pinky_3_S", "pinky_2_S", "finger5-3.S", "finger5-3.S.tail"),
    ("thigh_S", "hip", "upperleg01.S", "lowerleg01.S"),
    ("knee_S", "thigh_S", "lowerleg01.S", "foot.S"),
    ("ankle_S", "knee_S", "foot.S", "toe3-1.S"),
    ("ball_S", "ankle_S", "toe3-1.S", "toe3-3.S.tail"),
    ("toe_S", "ball_S", "toe3-3.S.tail", ""),
];

/// The rig bones and, per template bone, the rig bone its weights go to.
pub struct Rig {
    pub bones: Vec<RigBone>,
    pub of_template: Vec<usize>,
}

fn joint_ref(a: &HumanAsset, s: &str) -> Result<usize, String> {
    let (bone, end) = match s.strip_suffix(".tail") {
        Some(b) => (b, "tail"),
        None => (s, "head"),
    };
    let n = format!("{bone}____{end}");
    a.joints
        .iter()
        .position(|j| j.0 == n)
        .ok_or_else(|| format!("template has no joint `{n}`"))
}

/// The game rig (`full` false) or the whole template skeleton.
pub fn rig(a: &HumanAsset, full: bool) -> Result<Rig, String> {
    let tb = &a.bones;
    if full {
        let name = |s: &str| s.replace(".L", "_l").replace(".R", "_r");
        let bones = tb
            .iter()
            .map(|b| RigBone {
                name: name(&b.name),
                parent: (b.parent >= 0).then_some(b.parent as usize),
                head: b.head as usize,
                tail: Some(b.tail as usize),
            })
            .collect();
        return Ok(Rig {
            bones,
            of_template: (0..tb.len()).collect(),
        });
    }
    let mut bones: Vec<RigBone> = Vec::new();
    // Template bone -> rig bone for the heads.
    let mut own: HashMap<String, usize> = HashMap::new();
    for &(n, p, h, t) in GAME {
        let sides: &[(&str, &str)] = if n.contains("_S") {
            &[("_l", ".L"), ("_r", ".R")]
        } else {
            &[("", "")]
        };
        for (rs, ts) in sides {
            let rn = n.replace("_S", rs);
            let rp = p.replace("_S", rs);
            let head = h.replace(".S", ts);
            let tail = t.replace(".S", ts);
            let parent = if rp.is_empty() {
                None
            } else {
                Some(
                    bones
                        .iter()
                        .position(|b| b.name == rp)
                        .ok_or_else(|| format!("game rig: `{rn}` before its parent"))?,
                )
            };
            let hi = joint_ref(a, &head)?;
            let ti = if tail.is_empty() {
                None
            } else {
                Some(joint_ref(a, &tail)?)
            };
            if !head.ends_with(".tail") {
                own.insert(head.clone(), bones.len());
            }
            bones.push(RigBone {
                name: rn,
                parent,
                head: hi,
                tail: ti,
            });
        }
    }
    // Every template bone: itself if kept, toes onto the ball, else its
    // nearest kept ancestor.
    let mut of = vec![0usize; tb.len()];
    for (i, b) in tb.iter().enumerate() {
        if b.name.starts_with("toe") {
            let side = if b.name.ends_with(".R") { "_r" } else { "_l" };
            of[i] = bones
                .iter()
                .position(|x| x.name == format!("ball{side}"))
                .unwrap_or(0);
            continue;
        }
        let mut j = i as i32;
        of[i] = loop {
            if j < 0 {
                break 0;
            }
            if let Some(&r) = own.get(&tb[j as usize].name) {
                break r;
            }
            j = tb[j as usize].parent as i32;
        };
    }
    Ok(Rig {
        bones,
        of_template: of,
    })
}

/// Rounds of whole-body weight diffusion (each half a step).
const WEIGHT_PASSES: usize = 8;

/// Diffuses the skin weights over the body skin (uniform Laplacian,
/// `passes` rounds of half a step), so a joint's transition spans several
/// edge rings instead of one: the template's weights switch bones within an
/// edge or two at the crotch, the armpit and the knuckles, which stretches
/// those edges past 2x under a 30 degree swing.
fn smooth_weights(a: &HumanAsset, sw: Vec<Sw>, passes: usize, group: &[u8]) -> Vec<Sw> {
    if passes == 0 {
        return sw;
    }
    let mut nb: Vec<Vec<u32>> = vec![Vec::new(); a.verts.len()];
    for f in a.faces.iter().filter(|f| f.group == 0) {
        for k in 0..4 {
            let (x, y) = (f.v[k] as u32, f.v[(k + 1) % 4] as u32);
            nb[x as usize].push(y);
            nb[y as usize].push(x);
        }
    }
    for n in nb.iter_mut() {
        n.sort_unstable();
        n.dedup();
    }
    let mut cur = sw;
    for _ in 0..passes {
        let next: Vec<Sw> = (0..cur.len())
            .map(|i| {
                // Digits keep the template's weights (diffusing them softens
                // the knuckles into a rubber curl); the rest mixes only with
                // neighbours that are not on a digit.
                let g = group.get(i).copied().unwrap_or(0);
                if g != 0 {
                    return cur[i].clone();
                }
                let same: Vec<u32> = nb[i]
                    .iter()
                    .copied()
                    .filter(|&n| group.get(n as usize).copied().unwrap_or(0) == g)
                    .collect();
                if same.is_empty() {
                    return cur[i].clone();
                }
                let k = 0.5 / same.len() as f32;
                let mut acc: Sw = cur[i].iter().map(|&(j, w)| (j, w * 0.5)).collect();
                for &n in &same {
                    acc.extend(cur[n as usize].iter().map(|&(j, w)| (j, w * k)));
                }
                let (j, w) = top4(acc);
                j.iter()
                    .zip(w)
                    .filter(|x| x.1 > 1e-4)
                    .map(|(&j, w)| (j, w))
                    .collect()
            })
            .collect();
        cur = next;
    }
    cur
}

/// Between the legs the template hands a vertex 2 mm off the midline 15%
/// of its thigh and its twin across the midline 15% of the other thigh, so
/// a stride tears that 2 mm edge 3-4x. Below the hip joints, each thigh's
/// weight fades to zero toward the midline over `width` (smoothstep in
/// |x|, none on the far side) and the rest goes to the hip: the inner
/// thighs then shear over centimetres, not millimetres.
fn crotch_ramp(a: &HumanAsset, r: &Rig, mut sw: Vec<Sw>) -> Vec<Sw> {
    let bone = |n: &str| r.bones.iter().position(|b| b.name == n);
    let (Some(hip), Some(tl), Some(tr)) = (bone("hip"), bone("thigh_l"), bone("thigh_r")) else {
        return sw;
    };
    let pos: Vec<Vec3> = a.verts.iter().map(|v| Vec3::from(*v)).collect();
    let jp = joint_points(a, &pos);
    let (hl, hr) = (jp[r.bones[tl].head], jp[r.bones[tr].head]);
    let width = 0.04;
    let top = (hl.y + hr.y) * 0.5;
    let fade = 0.05;
    for f in a.faces.iter().filter(|f| f.group == 0) {
        for &v in &f.v {
            let p = pos[v as usize];
            if p.y > top || p.x.abs() > width {
                continue;
            }
            // Full effect below the joint by `fade`, none at the joint.
            let depth = ((top - p.y) / fade).clamp(0.0, 1.0);
            let t = (p.x.abs() / width).clamp(0.0, 1.0);
            let keep = t * t * (3.0 - 2.0 * t);
            let w = &mut sw[v as usize];
            let mut freed = 0.0;
            for (j, x) in w.iter_mut() {
                let j = *j as usize;
                let same = (j == tl && p.x >= 0.0) || (j == tr && p.x < 0.0);
                let other = (j == tl && p.x < 0.0) || (j == tr && p.x >= 0.0);
                let k = if same {
                    keep
                } else if other {
                    0.0
                } else {
                    continue;
                };
                let k = 1.0 - depth * (1.0 - k);
                freed += *x * (1.0 - k);
                *x *= k;
            }
            if freed > 0.0 {
                w.push((hip as u16, freed));
            }
        }
    }
    sw
}

/// Extra weight diffusion round the joints that fold hardest (the
/// armpits, elbows, knees): (bone whose head is the centre, radius m,
/// passes).
const FOLDS: &[(&str, f32, usize)] = &[
    ("shoulder_l", 0.10, 12),
    ("shoulder_r", 0.10, 12),
    ("elbow_l", 0.06, 16),
    ("elbow_r", 0.06, 16),
    ("wrist_l", 0.045, 8),
    ("wrist_r", 0.045, 8),
    ("knee_l", 0.07, 8),
    ("knee_r", 0.07, 8),
];

fn regional_smooth(a: &HumanAsset, r: &Rig, sw: Vec<Sw>) -> Vec<Sw> {
    let pos: Vec<Vec3> = a.verts.iter().map(|v| Vec3::from(*v)).collect();
    let jp = joint_points(a, &pos);
    let mut cur = sw;
    let k = 1.6;
    for &(b, rad, passes) in FOLDS {
        let Some(bi) = r.bones.iter().position(|x| x.name == b) else {
            continue;
        };
        let c = jp[r.bones[bi].head];
        let rad = rad * k;
        let smoothed = smooth_weights(a, cur.clone(), passes, &[]);
        for (i, p) in pos.iter().enumerate() {
            let t = p.distance(c) / rad;
            if t >= 1.0 {
                continue;
            }
            // Blend toward the smoothed weights, full at the centre.
            let f = 1.0 - t * t * (3.0 - 2.0 * t);
            let mut acc: Sw = cur[i].iter().map(|&(j, w)| (j, w * (1.0 - f))).collect();
            acc.extend(smoothed[i].iter().map(|&(j, w)| (j, w * f)));
            let (j, w) = top4(acc);
            cur[i] = j
                .iter()
                .zip(w)
                .filter(|x| x.1 > 1e-4)
                .map(|(&j, w)| (j, w))
                .collect();
        }
    }
    cur
}

/// Template vertices of the faces in group `g`.
fn group_verts(a: &HumanAsset, g: &str) -> Vec<u16> {
    let Some(gi) = a.groups.iter().position(|x| x == g) else {
        return Vec::new();
    };
    let mut vs: Vec<u16> = a
        .faces
        .iter()
        .filter(|f| f.group as usize == gi)
        .flat_map(|f| f.v)
        .collect();
    vs.sort_unstable();
    vs.dedup();
    vs
}

/// The skin weights per template vertex for the game rig (`full` false)
/// or the whole skeleton. They depend only on the template, so they are
/// computed once per rig kind.
fn template_weights(a: &HumanAsset, r: &Rig, full: bool) -> Arc<Vec<Sw>> {
    static CACHE: Mutex<[Option<Arc<Vec<Sw>>>; 2]> = Mutex::new([None, None]);
    if let Ok(c) = CACHE.lock()
        && let Some(w) = &c[full as usize]
    {
        return w.clone();
    }
    let def_rig_full = full;
    let mut sw: Vec<Sw> = vec![Vec::new(); a.verts.len()];
    for (bi, ws) in a.weights.iter().enumerate() {
        let rb = r.of_template[bi] as u16;
        for &(v, w) in ws {
            sw[v as usize].push((rb, w));
        }
    }
    let sw: Vec<Sw> = sw
        .into_iter()
        .map(|s| {
            let (j, w) = top4(s);
            j.iter()
                .zip(w)
                .filter(|x| x.1 > 0.0)
                .map(|(&j, w)| (j, w))
                .collect()
        })
        .collect();
    let sw = if def_rig_full {
        sw
    } else {
        // Vertices on a digit, by digit.
        let digit: Vec<u8> = r
            .bones
            .iter()
            .map(|b| {
                ["thumb", "index", "middle", "ring", "pinky"]
                    .iter()
                    .position(|f| b.name.starts_with(f))
                    .map_or(0, |i| {
                        i as u8 + 1 + if b.name.ends_with("_r") { 5 } else { 0 }
                    })
            })
            .collect();
        // The face (all on the head bones) keeps its weights too: diffusing
        // the neck up into the lips would part them from the rigid teeth.
        let head: Vec<bool> = r
            .bones
            .iter()
            .map(|b| b.name == "head" || b.name == "jaw" || b.name.starts_with("eye_"))
            .collect();
        let frozen: Vec<u8> = sw
            .iter()
            .map(|w| {
                if w.iter()
                    .filter(|x| head[x.0 as usize])
                    .map(|x| x.1)
                    .sum::<f32>()
                    > 0.98
                {
                    return 99;
                }
                w.iter()
                    .filter(|&&(j, x)| x > 0.01 && digit[j as usize] > 0)
                    .max_by(|a, b| a.1.total_cmp(&b.1))
                    .map_or(0, |&(j, _)| digit[j as usize])
            })
            .collect();
        let sw = smooth_weights(a, crotch_ramp(a, r, sw), WEIGHT_PASSES, &frozen);
        regional_smooth(a, r, sw)
    };
    let sw = rigid_mouth(a, r, sw);
    let w = Arc::new(sw);
    if let Ok(mut c) = CACHE.lock() {
        c[full as usize] = Some(w.clone());
    }
    w
}

/// The mouth helpers ride one bone each: the tongue and lower teeth the
/// jaw, the upper teeth the head (the template spreads the tongue over its
/// own bones, which the game rig merges, and leaves the teeth on facial
/// helpers).
fn rigid_mouth(a: &HumanAsset, r: &Rig, mut sw: Vec<Sw>) -> Vec<Sw> {
    let bone = |n: &str| r.bones.iter().position(|b| b.name == n);
    let (Some(jaw), Some(head)) = (bone("jaw"), bone("head")) else {
        return sw;
    };
    for (g, b) in [
        ("helper-tongue", jaw),
        ("helper-lower-teeth", jaw),
        ("helper-upper-teeth", head),
    ] {
        // The full rig keeps the tongue's own bones (children of the jaw).
        if r.bones.len() > 100 && g == "helper-tongue" {
            continue;
        }
        for v in group_verts(a, g) {
            sw[v as usize] = vec![(b as u16, 1.0)];
        }
    }
    sw
}

/// Expression units move the lips, jaw and teeth but not the tongue: carry
/// the tongue along with the lower teeth (the best rigid motion of the
/// teeth in the side plane, plus their mean sideways shift).
fn tongue_follows(a: &HumanAsset, d: &mut [Vec3]) {
    let teeth = group_verts(a, "helper-lower-teeth");
    let tongue = group_verts(a, "helper-tongue");
    if teeth.len() < 3 || tongue.is_empty() {
        return;
    }
    let n = teeth.len() as f32;
    let p0: Vec<Vec3> = teeth
        .iter()
        .map(|&v| Vec3::from(a.verts[v as usize]))
        .collect();
    let p1: Vec<Vec3> = teeth
        .iter()
        .zip(&p0)
        .map(|(&v, p)| *p + d[v as usize])
        .collect();
    let c0 = p0.iter().copied().sum::<Vec3>() / n;
    let c1 = p1.iter().copied().sum::<Vec3>() / n;
    if (c1 - c0).length() < 1e-6 && p0.iter().zip(&p1).all(|(x, y)| x.distance(*y) < 1e-6) {
        return;
    }
    // 2D Procrustes in (z, y): the jaw hinges about x.
    let (mut sc, mut ss) = (0.0f32, 0.0f32);
    for (x, y) in p0.iter().zip(&p1) {
        let (a0, b0) = (x.z - c0.z, x.y - c0.y);
        let (a1, b1) = (y.z - c1.z, y.y - c1.y);
        sc += a0 * a1 + b0 * b1;
        ss += a0 * b1 - b0 * a1;
    }
    let th = ss.atan2(sc);
    let (sn, cs) = th.sin_cos();
    // Units that already carry the tongue (or stick it out) move it about
    // as far as the teeth: only a tongue left behind is carried.
    let carried = |p: Vec3| {
        let (a0, b0) = (p.z - c0.z, p.y - c0.y);
        Vec3::new(
            p.x + (c1.x - c0.x),
            c1.y + sn * a0 + cs * b0,
            c1.z + cs * a0 - sn * b0,
        ) - p
    };
    let nt = tongue.len() as f32;
    let own = tongue.iter().map(|&v| d[v as usize].length()).sum::<f32>() / nt;
    let follow = tongue
        .iter()
        .map(|&v| carried(Vec3::from(a.verts[v as usize])).length())
        .sum::<f32>()
        / nt;
    if own > 0.5 * follow {
        return;
    }
    for &v in &tongue {
        d[v as usize] += carried(Vec3::from(a.verts[v as usize]));
    }
}

/// Joint ref positions for evaluated vertex positions.
fn joint_points(a: &HumanAsset, pos: &[Vec3]) -> Vec<Vec3> {
    a.joints
        .iter()
        .map(|(_, vs)| {
            if vs.is_empty() {
                return Vec3::ZERO;
            }
            vs.iter().map(|&v| pos[v as usize]).sum::<Vec3>() / vs.len() as f32
        })
        .collect()
}

/// The body's triangles at detail 0 (the quad cage split in two).
pub const CAGE_TRIS: usize = 26_756;

/// The subdivision level a build runs: `detail`, except that a `tris`
/// budget without `bake` starts from the coarsest level that still has at
/// least that many triangles (detail 0 is 26,756, each level x4), so a
/// light body decimates the cage, not a 430k-triangle surface, and a budget
/// at or above the level's own count is not decimated at all.
fn effective_detail(def: &crate::body::BodyDef) -> usize {
    let asked = (def.detail.unwrap_or(0) as usize).min(2);
    match def.tris {
        Some(t) if !def.bake => (0..=asked)
            .find(|&k| CAGE_TRIS << (2 * k) >= t as usize)
            .unwrap_or(asked),
        _ => asked,
    }
}

/// How a subdivided body is fitted back to the item: Catmull-Clark pulls
/// the surface in (the crown down, the soles up, ~1.5 mm at detail 1), so
/// the result is scaled about the origin by `scale` and lifted by `dy` to
/// stand on y = 0 at the cage's stature; `joints` are the joint refs
/// on the subdivided, fitted surface (what the rig and the mesh share).
struct Fit {
    scale: f32,
    dy: f32,
    joints: Vec<Vec3>,
}

static FITS: Mutex<Vec<(u64, Arc<Fit>)>> = Mutex::new(Vec::new());

fn fit(def: &crate::body::BodyDef) -> Result<Arc<Fit>, String> {
    let a = asset::load()?;
    let ev = evaluate(def)?;
    let n = effective_detail(def);
    let key = eval_key(def) ^ (n as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    if let Ok(c) = FITS.lock()
        && let Some(e) = c.iter().find(|e| e.0 == key)
    {
        return Ok(e.1.clone());
    }
    let f = if n == 0 {
        Fit {
            scale: 1.0,
            dy: 0.0,
            joints: joint_points(&a, &ev.pos),
        }
    } else {
        let (lo0, hi0) = body_extent_y(&a, &ev.pos);
        let levels = level(&a, n);
        let mut pos = ev.pos.clone();
        for l in &levels {
            pos = apply_v(&l.stencil, &pos);
        }
        let Some(last) = levels.last() else {
            return Err("no subdivision level".into());
        };
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        for (fi, q) in last.stencil.faces.iter().enumerate() {
            if last.groups[fi] == 0 {
                for &v in q {
                    lo = lo.min(pos[v as usize].y);
                    hi = hi.max(pos[v as usize].y);
                }
            }
        }
        let scale = (hi0 - lo0) / (hi - lo).max(1e-6);
        let dy = -lo * scale;
        let moved: Vec<Vec3> = pos[..a.verts.len()]
            .iter()
            .map(|p| *p * scale + Vec3::Y * dy)
            .collect();
        Fit {
            scale,
            dy,
            joints: joint_points(&a, &moved),
        }
    };
    let f = Arc::new(f);
    if let Ok(mut c) = FITS.lock() {
        c.retain(|x| x.0 != key);
        c.push((key, f.clone()));
        if c.len() > 12 {
            c.remove(0);
        }
    }
    Ok(f)
}

/// The skeleton bones a human item makes (name, parent, head, tail,
/// radius), in body space.
pub fn rig_raw(def: &crate::body::BodyDef) -> Result<Vec<crate::anim::auto::RawBone>, String> {
    let a = asset::load()?;
    let r = rig(&a, def.rig_full)?;
    let jp = fit(def)?.joints.clone();
    Ok(r.bones
        .iter()
        .map(|b| {
            let h = jp[b.head];
            let t = b.tail.map(|t| jp[t]).filter(|t| t.distance(h) > 1e-5);
            let len = t.map_or(0.05, |t| t.distance(h));
            (
                b.name.clone(),
                b.parent.map(|p| r.bones[p].name.clone()),
                h,
                t,
                Some((len * 0.3).clamp(0.004, 0.12)),
            )
        })
        .collect())
}

/// Rest frames and joint limits of the game rig (bone name, rest rotation
/// = the bone's local axes in world space, limits in degrees about those
/// axes). Legs, spine, neck, head and jaw stay world-aligned (identity),
/// so biped clips (`thigh {"x": 30}`) keep their meaning. Arms and digits
/// get anatomical frames, because the template's A-pose arms point down,
/// out and forward, where no world axis is a hinge: local +y runs along
/// the bone (head to tail) and a positive local x rotation flexes it (the
/// elbow closes, the wrist and fingers curl toward the palm, the shoulder
/// raises the arm forward), on both sides. Limits: knees x 0..150 (world
/// x, +x bends the shin back), elbows x 0..150, fingers x -10..100 with y
/// and z nearly locked, so IK can't bend them backward.
pub fn bone_frames(
    def: &crate::body::BodyDef,
) -> Result<Vec<(String, bevy::math::Quat, Option<crate::anim::Limits>)>, String> {
    use bevy::math::{Mat3, Quat};
    if def.rig_full {
        return Ok(Vec::new());
    }
    let bones = rig_raw(def)?;
    let at = |n: &str| bones.iter().find(|b| b.0 == n).map(|b| b.2);
    let tail = |n: &str| bones.iter().find(|b| b.0 == n).and_then(|b| b.3);
    let frame = |y: Vec3, x: Vec3| -> Option<Quat> {
        let y = y.try_normalize()?;
        let x = (x - y * x.dot(y)).try_normalize()?;
        let z = x.cross(y);
        Some(Quat::from_mat3(&Mat3::from_cols(x, y, z)).normalize())
    };
    let lim = |x: [f32; 2], y: [f32; 2], z: [f32; 2]| crate::anim::Limits {
        x: Some(x),
        y: Some(y),
        z: Some(z),
    };
    let mut out = Vec::new();
    for (s, sign) in [("_l", 1.0f32), ("_r", -1.0)] {
        let n = |b: &str| format!("{b}{s}");
        let (Some(sh), Some(el), Some(wr), Some(ha)) = (
            at(&n("shoulder")),
            at(&n("elbow")),
            at(&n("wrist")),
            at(&n("hand")),
        ) else {
            continue;
        };
        // Elbow: flexion turns the forearm toward the upper arm.
        let fore = wr - el;
        let up = sh - el;
        let k_elbow = fore.cross(up - fore.normalize() * up.dot(fore.normalize()));
        // The A-pose elbow is already bent: x 0 is that rest bend, so the
        // straight arm is at minus the rest angle.
        let rest_bend = (el - sh).angle_between(fore).to_degrees();
        if let Some(q) = frame(fore, k_elbow) {
            out.push((
                n("elbow"),
                q,
                Some(lim(
                    [-rest_bend - 3.0, 150.0 - rest_bend],
                    [-90.0, 90.0],
                    [-10.0, 10.0],
                )),
            ));
        }
        // Shoulder: the same hinge axis, +x raises the arm forward.
        if let Some(q) = frame(el - sh, k_elbow) {
            out.push((n("shoulder"), q, None));
        }
        // The palm faces in (toward the thigh): its normal points to -x on
        // the left, +x on the right, kept square to the knuckle line.
        let (Some(i1), Some(p1)) = (at(&n("index_1")), at(&n("pinky_1"))) else {
            continue;
        };
        let knuckles = (i1 - p1).normalize_or(Vec3::Z);
        let hand_dir = (ha - wr).normalize_or(Vec3::NEG_Y);
        let mut palm = hand_dir.cross(knuckles).normalize_or(Vec3::NEG_X * sign);
        if palm.x * sign > 0.0 {
            palm = -palm;
        }
        let curl = |d: Vec3| d.cross(palm);
        for b in ["wrist", "hand"] {
            let (Some(h), Some(t)) = (at(&n(b)), tail(&n(b))) else {
                continue;
            };
            if let Some(q) = frame(t - h, curl(t - h)) {
                let l = (b == "wrist").then(|| lim([-70.0, 80.0], [-80.0, 80.0], [-30.0, 30.0]));
                out.push((n(b), q, l));
            }
        }
        for f in ["index", "middle", "ring", "pinky", "thumb"] {
            for k in 1..=3 {
                let b = n(&format!("{f}_{k}"));
                let (Some(h), Some(t)) = (at(&b), tail(&b)) else {
                    continue;
                };
                let d = t - h;
                let axis = if f == "thumb" {
                    // The thumb curls across the palm toward the middle
                    // knuckle.
                    let m1 = at(&n("middle_1")).unwrap_or(ha);
                    d.cross(m1 - h)
                } else {
                    curl(d)
                };
                let Some(q) = frame(d, axis) else {
                    continue;
                };
                let l = if f == "thumb" {
                    None
                } else if k == 1 {
                    Some(lim([-20.0, 95.0], [-25.0, 25.0], [-10.0, 10.0]))
                } else {
                    Some(lim([-5.0, 110.0], [-5.0, 5.0], [-5.0, 5.0]))
                };
                out.push((b, q, l));
            }
        }
    }
    for k in ["knee_l", "knee_r"] {
        out.push((
            k.to_string(),
            Quat::IDENTITY,
            Some(lim([0.0, 150.0], [-15.0, 15.0], [-10.0, 10.0])),
        ));
    }
    Ok(out)
}

/// The joints (anchors): every rig bone's head.
pub fn joints(def: &crate::body::BodyDef) -> Result<Vec<crate::body::Joint>, String> {
    Ok(rig_raw(def)?
        .into_iter()
        .map(|(name, parent, at, _, r)| crate::body::Joint {
            name,
            parent,
            at,
            radius: r.unwrap_or(0.02),
        })
        .collect())
}

// ------------------------------------------------------------- building

/// Warp centres: a joint name (`"head"`, or `"<this item>:head"`), an
/// anchor on another part (`"desk:top"`, a joint of another body too), or
/// anchor words on this body's own box (`"top-front"`).
fn resolve_warp_anchors(
    name: &str,
    warps: &[crate::deform::Deform],
    joints: &[(String, [f32; 3])],
    cage: &[Vec3],
    lookup: Option<&crate::sdf::Lookup>,
) -> Result<Vec<crate::deform::Deform>, String> {
    let joint = |n: &str| joints.iter().find(|j| j.0 == n).map(|j| j.1);
    let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for p in cage {
        lo = lo.min(*p);
        hi = hi.max(*p);
    }
    warps
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let mut w = w.clone();
            let Some(crate::deform::WarpAt::Anchor(at)) = &w.at else {
                return Ok(w);
            };
            let p = if let Some(p) = joint(at) {
                p
            } else if let Some((pn, words)) = at.split_once(':') {
                if pn == name {
                    match joint(words) {
                        Some(p) => p,
                        None => crate::commands::anchor_point(words, lo, hi)
                            .map_err(|e| format!("warps[{i}]: at `{at}`: {e}"))?,
                    }
                } else {
                    let Some(look) = lookup else {
                        return Err(format!("warps[{i}]: at `{at}`: no parts to read"));
                    };
                    let part = look(pn).map_err(|e| format!("warps[{i}]: at `{at}`: {e}"))?;
                    match part.joint(words) {
                        Some(j) => j.to_array(),
                        None => {
                            let (plo, phi) = part.bounds();
                            crate::commands::anchor_point(words, plo, phi)
                                .map_err(|e| format!("warps[{i}]: at `{at}`: {e}"))?
                        }
                    }
                }
            } else {
                crate::commands::anchor_point(at, lo, hi).map_err(|e| {
                    format!(
                        "warps[{i}]: at `{at}` is not a joint or an anchor on the body's box ({e})"
                    )
                })?
            };
            w.at = Some(crate::deform::WarpAt::Point(p));
            Ok(w)
        })
        .collect()
}

fn falloff(kind: &str, t: f32) -> f32 {
    if t >= 1.0 {
        return 0.0;
    }
    match kind {
        "linear" => 1.0 - t,
        "sharp" => (1.0 - t).powi(3),
        _ => {
            let u = 1.0 - t * t;
            u * u
        }
    }
}

/// Vertex normals of a quad cage (area weighted).
fn cage_normals(pos: &[Vec3], quads: &[[u32; 4]]) -> Vec<Vec3> {
    let mut n = vec![Vec3::ZERO; pos.len()];
    for q in quads {
        let p = q.map(|i| pos[i as usize]);
        let f = (p[2] - p[0]).cross(p[3] - p[1]);
        for &i in q {
            n[i as usize] += f;
        }
    }
    n.iter().map(|v| v.normalize_or_zero()).collect()
}

/// Local sculpt warps as vertex displacements on the cage.
fn warp(
    pos: &mut [Vec3],
    quads: &[[u32; 4]],
    warps: &[crate::deform::Deform],
) -> Result<(), String> {
    let mut nb: Option<Vec<Vec<u32>>> = None;
    for w in warps {
        let Some(crate::deform::WarpAt::Point(at)) = &w.at else {
            return Err(format!(
                "warps: {} needs `at` as a point or a joint name",
                w.mode
            ));
        };
        let at = Vec3::from(*at);
        let r = w.radius.unwrap_or(0.1).max(1e-5);
        let fk = w.falloff.as_deref().unwrap_or("smooth");
        let amount = w.amount.unwrap_or(0.0);
        let f = |p: Vec3| falloff(fk, p.distance(at) / r);
        match w.mode.as_str() {
            "inflate" => {
                let n = cage_normals(pos, quads);
                for (p, n) in pos.iter_mut().zip(n) {
                    *p += n * amount * f(*p);
                }
            }
            "push" => {
                let d = Vec3::from(w.direction.unwrap_or([0.0, 0.0, 1.0])).normalize_or_zero();
                for p in pos.iter_mut() {
                    *p += d * amount * f(*p);
                }
            }
            "pinch" => {
                for p in pos.iter_mut() {
                    *p += (at - *p) * (amount * f(*p)).clamp(0.0, 1.0);
                }
            }
            "flatten" => {
                let n = Vec3::from(w.normal.unwrap_or([0.0, 1.0, 0.0])).normalize_or_zero();
                for p in pos.iter_mut() {
                    let d = (*p - at).dot(n);
                    *p -= n * d * (amount * f(*p)).clamp(0.0, 1.0);
                }
            }
            "smooth" => {
                let nb = nb.get_or_insert_with(|| {
                    let mut nb: Vec<Vec<u32>> = vec![Vec::new(); pos.len()];
                    for q in quads {
                        for i in 0..4 {
                            let (a, b) = (q[i], q[(i + 1) % 4]);
                            nb[a as usize].push(b);
                            nb[b as usize].push(a);
                        }
                    }
                    for v in nb.iter_mut() {
                        v.sort_unstable();
                        v.dedup();
                    }
                    nb
                });
                let steps = ((w.size.unwrap_or(r / 3.0) / 0.005).round() as usize).clamp(1, 20);
                for _ in 0..steps {
                    let old = pos.to_vec();
                    for (i, p) in pos.iter_mut().enumerate() {
                        let k = (amount * f(old[i])).clamp(0.0, 1.0);
                        if k <= 0.0 || nb[i].is_empty() {
                            continue;
                        }
                        let avg = nb[i].iter().map(|&j| old[j as usize]).sum::<Vec3>()
                            / nb[i].len() as f32;
                        *p = p.lerp(avg, k);
                    }
                }
            }
            m => {
                return Err(format!(
                    "warps: `{m}` is a field warp; a human takes local warps only ({})",
                    crate::deform::LOCAL_MODES.join(", ")
                ));
            }
        }
    }
    Ok(())
}

/// Sparse skin weights (up to 8 kept while subdividing).
type Sw = Vec<(u16, f32)>;

fn top4(mut acc: Sw) -> ([u16; 4], [f32; 4]) {
    acc.sort_by_key(|a| a.0);
    let mut merged: Sw = Vec::with_capacity(acc.len());
    for (j, w) in acc {
        match merged.last_mut() {
            Some(m) if m.0 == j => m.1 += w,
            _ => merged.push((j, w)),
        }
    }
    merged.sort_by(|a, b| b.1.total_cmp(&a.1));
    let sum: f32 = merged.iter().take(4).map(|m| m.1).sum();
    let mut j = [0u16; 4];
    let mut w = [0f32; 4];
    for (k, m) in merged.iter().take(4).enumerate() {
        j[k] = m.0;
        w[k] = if sum > 0.0 { m.1 / sum } else { 0.0 };
    }
    if sum <= 0.0 {
        w[0] = 1.0;
    }
    (j, w)
}

/// One subdivision level's cached topology over the template's kept
/// faces (eyes aside: they are made fresh).
struct Level {
    stencil: crate::smooth::Stencil,
    /// Face-varying uv stencil: new uv index -> (old uv, weight) rows.
    uv_rows: Vec<Vec<(u32, f32)>>,
    /// Per new face, its four uv indices.
    uv_faces: Vec<[u32; 4]>,
    groups: Vec<u8>,
}

type Topo = (Vec<[u32; 4]>, Vec<[u32; 4]>, Vec<u8>);

fn base_topology(a: &HumanAsset) -> Topo {
    let mut q = Vec::new();
    let mut t = Vec::new();
    let mut g = Vec::new();
    for f in &a.faces {
        if a.groups[f.group as usize].ends_with("-eye") {
            continue;
        }
        q.push(f.v.map(u32::from));
        t.push(f.t);
        g.push(f.group);
    }
    (q, t, g)
}

static LEVELS: Mutex<Vec<Arc<Level>>> = Mutex::new(Vec::new());

/// The topology after `n` levels (cached; level k builds on level k - 1).
fn level(a: &HumanAsset, n: usize) -> Vec<Arc<Level>> {
    let mut c = match LEVELS.lock() {
        Ok(c) => c,
        Err(p) => p.into_inner(),
    };
    while c.len() < n {
        let (quads, uvf, groups, nv, nuv): (Vec<[u32; 4]>, Vec<[u32; 4]>, Vec<u8>, usize, usize) =
            match c.last() {
                None => {
                    let (q, t, g) = base_topology(a);
                    (q, t, g, a.verts.len(), a.uvs.len())
                }
                Some(l) => (
                    l.stencil
                        .faces
                        .iter()
                        .map(|f| [f[0], f[1], f[2], f[3]])
                        .collect(),
                    l.uv_faces.clone(),
                    l.groups.clone(),
                    l.stencil.len(),
                    l.uv_rows.len(),
                ),
            };
        let faces: Vec<Vec<u32>> = quads.iter().map(|q| q.to_vec()).collect();
        let stencil = crate::smooth::catmull_clark_stencil(nv, &faces);
        // Uvs: corners keep theirs, an edge's midpoint (shared by faces that
        // share both uv corners) and each face's centre are averages.
        let mut uv_rows: Vec<Vec<(u32, f32)>> = (0..nuv as u32).map(|i| vec![(i, 1.0)]).collect();
        let mut edge: HashMap<(u32, u32), u32> = HashMap::new();
        let mut centre: Vec<u32> = Vec::with_capacity(uvf.len());
        for f in &uvf {
            centre.push(uv_rows.len() as u32);
            uv_rows.push(f.iter().map(|&u| (u, 0.25)).collect());
        }
        let mut uv_faces = Vec::with_capacity(stencil.faces.len());
        for fr in &stencil.from {
            let mut out = [0u32; 4];
            for (k, &(fi, ci, kind)) in fr.iter().enumerate() {
                let f = uvf[fi as usize];
                out[k] = match kind {
                    0 => f[ci as usize],
                    1 => {
                        let (u0, u1) = (f[ci as usize], f[(ci as usize + 1) % 4]);
                        let key = (u0.min(u1), u0.max(u1));
                        *edge.entry(key).or_insert_with(|| {
                            uv_rows.push(vec![(u0, 0.5), (u1, 0.5)]);
                            (uv_rows.len() - 1) as u32
                        })
                    }
                    _ => centre[fi as usize],
                };
            }
            uv_faces.push(out);
        }
        let groups = stencil.parent.iter().map(|&p| groups[p as usize]).collect();
        c.push(Arc::new(Level {
            stencil,
            uv_rows,
            uv_faces,
            groups,
        }));
    }
    c.iter().take(n).cloned().collect()
}

#[derive(Clone, Copy, Default)]
struct V3(Vec3);
impl std::ops::Add for V3 {
    type Output = V3;
    fn add(self, o: V3) -> V3 {
        V3(self.0 + o.0)
    }
}
impl std::ops::Mul<f32> for V3 {
    type Output = V3;
    fn mul(self, k: f32) -> V3 {
        V3(self.0 * k)
    }
}

fn apply_v(s: &crate::smooth::Stencil, v: &[Vec3]) -> Vec<Vec3> {
    let w: Vec<V3> = v.iter().map(|x| V3(*x)).collect();
    s.apply(&w).into_iter().map(|x| x.0).collect()
}

fn apply_w(s: &crate::smooth::Stencil, v: &[Sw]) -> Vec<Sw> {
    use rayon::prelude::*;
    (0..s.len())
        .into_par_iter()
        .map(|i| {
            let (a, b) = (s.offsets[i] as usize, s.offsets[i + 1] as usize);
            let mut acc: Sw = Vec::new();
            for &(j, w) in &s.rows[a..b] {
                acc.extend(v[j as usize].iter().map(|&(bn, x)| (bn, x * w)));
            }
            acc.sort_by_key(|a| a.0);
            let mut m: Sw = Vec::with_capacity(8);
            for (j, w) in acc {
                match m.last_mut() {
                    Some(l) if l.0 == j => l.1 += w,
                    _ => m.push((j, w)),
                }
            }
            m.sort_by(|a, b| b.1.total_cmp(&a.1));
            m.truncate(8);
            m
        })
        .collect()
}

/// A UV sphere (an eye) at `c`, radius `r`, facing +z (its uv u wraps
/// around z, v from front to back).
fn eye_sphere(c: Vec3, r: f32, rings: u32, segs: u32) -> (Vec<Vec3>, Vec<[f32; 2]>, Vec<u32>) {
    let mut v = Vec::new();
    let mut uv = Vec::new();
    for i in 0..=rings {
        let th = std::f32::consts::PI * i as f32 / rings as f32;
        for j in 0..=segs {
            let ph = std::f32::consts::TAU * j as f32 / segs as f32;
            // Pole at +z (the pupil).
            let d = Vec3::new(th.sin() * ph.cos(), th.sin() * ph.sin(), th.cos());
            v.push(c + d * r);
            // Azimuthal uvs round the pupil: the front pole at the
            // centre, the polar angle out to the rim (the back pole).
            let rr = 0.5 * th / std::f32::consts::PI;
            uv.push([0.5 + rr * ph.cos(), 0.5 - rr * ph.sin()]);
        }
    }
    let mut idx = Vec::new();
    let w = segs + 1;
    for i in 0..rings {
        for j in 0..segs {
            let (a, b, c2, d) = (
                i * w + j,
                i * w + j + 1,
                (i + 1) * w + j + 1,
                (i + 1) * w + j,
            );
            if i > 0 {
                idx.extend([a, d, b]);
            }
            if i + 1 < rings {
                idx.extend([b, d, c2]);
            }
        }
    }
    (v, uv, idx)
}

/// The iris radius in eye uvs: a 24 mm eyeball's ~11.5 mm iris spans
/// about 29 degrees from the front pole (uv radius = degrees / 360).
pub const IRIS_UV: f32 = 0.08;

/// The built-in eye material (card domain over the eye's azimuthal uvs:
/// uv [0.5, 0.5] is the pupil): sclera, an `iris` coloured iris with
/// noise fibres, a dark limbus ring at its rim, a `pupil`-scaled pupil and
/// a glossy clearcoat cornea.
pub fn eye_graph(iris: [f32; 4], pupil: f32) -> crate::material::MatGraph {
    use serde_json::json;
    let hex = |c: [f32; 3]| {
        let b = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
        format!("#{:02x}{:02x}{:02x}", b(c[0]), b(c[1]), b(c[2]))
    };
    let k = |f: f32| [iris[0] * f, iris[1] * f, iris[2] * f];
    let pr = (0.03 * pupil.clamp(0.4, 2.2)).min(IRIS_UV * 0.8);
    crate::material::MatGraph {
        domain: "card".into(),
        tile: 1.0,
        resolution: 512,
        nodes: vec![
            json!({"type": "circle", "name": "iris_sdf", "center": [0.5, 0.5], "radius": IRIS_UV}),
            json!({"type": "circle", "name": "pupil_sdf", "center": [0.5, 0.5], "radius": pr}),
            json!({"type": "onion", "name": "limbus_sdf", "input": "iris_sdf", "thickness": 0.006}),
            json!({"type": "mask", "name": "iris_m", "sdf": "iris_sdf", "mode": "fill"}),
            json!({"type": "mask", "name": "pupil_m", "sdf": "pupil_sdf", "mode": "fill"}),
            json!({"type": "mask", "name": "limbus_m", "sdf": "limbus_sdf", "mode": "fill"}),
            json!({"type": "noise", "name": "fibres", "size": 0.006, "octaves": 3, "seed": 3}),
            json!({"type": "ramp", "name": "iris_col", "input": "fibres",
                   "stops": [[0.0, hex(k(0.45))], [0.6, hex(k(1.0))], [1.0, hex(k(1.35))]]}),
            json!({"type": "noise", "name": "veins", "size": 0.05, "octaves": 2, "seed": 7}),
            json!({"type": "ramp", "name": "sclera", "input": "veins",
                   "stops": [[0.0, "#f3efe8"], [1.0, "#e6d6cc"]]}),
            json!({"type": "blend", "name": "c1", "a": "sclera", "b": "iris_col", "opacity": "iris_m"}),
            json!({"type": "blend", "name": "c2", "a": "c1", "b": hex(k(0.18)), "opacity": "limbus_m"}),
            json!({"type": "blend", "name": "c3", "a": "c2", "b": "#060504", "opacity": "pupil_m"}),
            json!({"type": "output", "name": "eye", "base_color": "c3", "roughness": 0.35,
                   "clearcoat": 1.0, "clearcoat_roughness": 0.03}),
        ],
        params: Default::default(),
    }
}

/// Gives a part a built-in procedural look (white factors over `graph`);
/// a `material` item on the part replaces it.
fn builtin_look(p: &mut Part, name: &str, graph: crate::material::MatGraph) {
    p.material.base_color = [1.0; 4];
    p.material.roughness = 1.0;
    p.material.metalness = 0.0;
    p.material.pbr = Some(crate::material::PbrRef {
        material: name.into(),
        params: Default::default(),
        scale: 1.0,
        graph: Some(Arc::new(graph)),
    });
}

/// The built-in skin look (card domain over the template's own uvs, so
/// the skin weights stay on unsplit vertices): `tone` (linear-ish sRGB)
/// mottled by two noise scales, a faint warm flush, roughness varying
/// 0.45..0.65 and a little diffuse transmission for subsurface glow.
pub fn skin_graph(tone: [f32; 3]) -> crate::material::MatGraph {
    use serde_json::json;
    let hex = |c: [f32; 3], f: f32| {
        let b = |x: f32| ((x * f).clamp(0.0, 1.0) * 255.0).round() as u8;
        format!("#{:02x}{:02x}{:02x}", b(c[0]), b(c[1]), b(c[2]))
    };
    let flush = [tone[0] * 1.02, tone[1] * 0.86, tone[2] * 0.84];
    crate::material::MatGraph {
        domain: "card".into(),
        tile: 1.0,
        resolution: 1024,
        nodes: vec![
            json!({"type": "noise", "name": "pores", "size": 0.004, "octaves": 2, "seed": 11}),
            json!({"type": "noise", "name": "mottle", "size": 0.06, "octaves": 4, "seed": 4}),
            json!({"type": "ramp", "name": "base", "input": "mottle",
                   "stops": [[0.0, hex(tone, 0.93)], [0.5, hex(tone, 1.0)], [1.0, hex(flush, 1.0)]]}),
            json!({"type": "blend", "name": "col", "a": "base", "b": hex(tone, 0.88), "opacity": 0.12}),
            json!({"type": "remap", "name": "rough", "input": "pores", "from": [0.0, 1.0], "to": [0.45, 0.65]}),
            json!({"type": "math", "name": "h", "op": "mul", "a": "pores", "b": 0.00015}),
            json!({"type": "output", "name": "skin", "base_color": "col", "roughness": "rough",
                   "height": "h", "transmission": 0.25}),
        ],
        params: Default::default(),
    }
}

/// The built-in teeth / tongue look: a fine two-tone noise,
/// `roughness`, and a wet `clearcoat`.
fn mouth_graph(cols: [&str; 2], roughness: f32, clearcoat: f32) -> crate::material::MatGraph {
    use serde_json::json;
    crate::material::MatGraph {
        // Card: drawn over the part's own (template) uvs, which a tile
        // material's metric remap would split, dropping the skin weights.
        domain: "card".into(),
        tile: 0.02,
        resolution: 128,
        nodes: vec![
            json!({"type": "noise", "name": "n", "size": 0.004, "octaves": 3, "seed": 5}),
            json!({"type": "ramp", "name": "c", "input": "n", "stops": [[0.0, cols[0]], [1.0, cols[1]]]}),
            json!({"type": "output", "name": "o", "base_color": "c", "roughness": roughness,
                   "clearcoat": clearcoat, "clearcoat_roughness": 0.1}),
        ],
        params: Default::default(),
    }
}

/// Smooth normals by position (shared across uv seams).
fn normals(verts: &[[f32; 3]], indices: &[u32], weld: &[u32], ngroups: usize) -> Vec<[f32; 3]> {
    let mut acc = vec![Vec3::ZERO; ngroups];
    for t in indices.as_chunks::<3>().0 {
        let p = t.map(|i| Vec3::from(verts[i as usize]));
        let n = (p[1] - p[0]).cross(p[2] - p[0]);
        for &i in t {
            acc[weld[i as usize] as usize] += n;
        }
    }
    weld.iter()
        .map(|&g| acc[g as usize].normalize_or(Vec3::Y).to_array())
        .collect()
}

/// Every part a human item makes: the body (`name`) and the extras its
/// `parts` asks for, each with its skin weights, joints and morphs.
pub fn build(name: &str, def: &crate::body::BodyDef) -> Result<Vec<Part>, String> {
    build_with(name, def, None)
}

/// [`build`] with the model's parts for `"part:anchor"` warp centres.
pub fn build_with(
    name: &str,
    def: &crate::body::BodyDef,
    lookup: Option<&crate::sdf::Lookup>,
) -> Result<Vec<Part>, String> {
    let a = asset::load()?;
    let ev = evaluate(def)?;
    let r = rig(&a, def.rig_full)?;
    let bone_names: Arc<Vec<String>> = Arc::new(r.bones.iter().map(|b| b.name.clone()).collect());
    let fitted = fit(def)?;
    let jp = fitted.joints.clone();
    let joint_list: Arc<Vec<(String, [f32; 3])>> = Arc::new(
        r.bones
            .iter()
            .map(|b| (b.name.clone(), jp[b.head].to_array()))
            .collect(),
    );
    if def.detail.unwrap_or(0) > 2 {
        return Err(format!(
            "detail: 0, 1 or 2, got {}",
            def.detail.unwrap_or(0)
        ));
    }
    let level_n = effective_detail(def);
    let extras = def
        .parts
        .clone()
        .unwrap_or_else(|| EXTRAS.iter().map(|s| s.to_string()).collect());
    for e in &extras {
        if !EXTRAS.contains(&e.as_str()) {
            return Err(format!("parts: `{e}` is not one of {}", EXTRAS.join(", ")));
        }
    }
    // Cage positions with warps.
    let (quads, _, _) = base_topology(&a);
    let mut cage = ev.pos.clone();
    let warps = crate::body::mirror_local_warps(resolve_warp_anchors(
        name,
        &def.warps,
        &joint_list,
        &cage,
        lookup,
    )?);
    warp(&mut cage, &quads, &warps)?;
    // Skin weights per template vertex (template data only: cached).
    let sw = template_weights(&a, &r, def.rig_full);
    // Morph targets: deltas against this item's own base.
    let mut morphs: Vec<(String, Vec<Vec3>)> = Vec::new();
    let p = Params::of(def)?;
    let mut base_d = vec![Vec3::ZERO; a.verts.len()];
    if !def.shapes.is_empty() {
        add_details(&mut base_d, &a, &p.shape, &p.expression, p.ethnicity)?;
    }
    for (k, s) in &def.shapes {
        // The shape's values replace the item's for the keys it names.
        let mut shape = p.shape.clone();
        shape.extend(s.shape.iter().map(|(n, v)| (n.clone(), *v)));
        let mut expr = p.expression.clone();
        expr.extend(s.expression.iter().map(|(n, v)| (n.clone(), *v)));
        let mut d = vec![Vec3::ZERO; a.verts.len()];
        add_details(&mut d, &a, &shape, &expr, p.ethnicity)
            .map_err(|e| format!("shapes.{k}: {e}"))?;
        let delta: Vec<Vec3> = (0..a.verts.len())
            .map(|i| (d[i] - base_d[i]) * ev.scale)
            .collect();
        morphs.push((k.clone(), delta));
    }
    // Subdivide.
    let levels = level(&a, level_n);
    let mut pos = cage;
    let mut weights: Vec<Sw> = sw.as_ref().clone();
    let mut deltas: Vec<Vec<Vec3>> = morphs.iter().map(|m| m.1.clone()).collect();
    let (base_q, base_t, base_g) = base_topology(&a);
    let mut faces: Vec<[u32; 4]> = base_q;
    let mut uvf: Vec<[u32; 4]> = base_t;
    let mut groups: Vec<u8> = base_g;
    let mut uvs: Vec<[f32; 2]> = a.uvs.clone();
    for l in &levels {
        pos = apply_v(&l.stencil, &pos);
        weights = apply_w(&l.stencil, &weights);
        for d in deltas.iter_mut() {
            *d = apply_v(&l.stencil, d);
        }
        uvs = l
            .uv_rows
            .iter()
            .map(|row| {
                let mut s = [0.0f32; 2];
                for &(u, w) in row {
                    s[0] += uvs[u as usize][0] * w;
                    s[1] += uvs[u as usize][1] * w;
                }
                s
            })
            .collect();
        faces = l
            .stencil
            .faces
            .iter()
            .map(|f| [f[0], f[1], f[2], f[3]])
            .collect();
        uvf = l.uv_faces.clone();
        groups = l.groups.clone();
    }
    // Subdivision pulls the surface in: fit it back to the stature.
    if !levels.is_empty() {
        for p in pos.iter_mut() {
            *p = *p * fitted.scale + Vec3::Y * fitted.dy;
        }
        for d in deltas.iter_mut() {
            for x in d.iter_mut() {
                *x *= fitted.scale;
            }
        }
    }
    let skin = |joints: Vec<[u16; 4]>, w: Vec<[f32; 4]>| {
        Arc::new(SkinWeights {
            bones: bone_names.clone(),
            joints,
            weights: w,
        })
    };
    // Split into parts by group.
    let make = |pname: &str, want: &[&str]| -> Part {
        let gids: Vec<u8> = a
            .groups
            .iter()
            .enumerate()
            .filter(|(_, g)| want.contains(&g.as_str()))
            .map(|(i, _)| i as u8)
            .collect();
        let mut qacc: HashMap<u32, Vec3> = HashMap::new();
        let mut map: HashMap<(u32, u32), u32> = HashMap::new();
        let mut verts = Vec::new();
        let mut puv = Vec::new();
        let mut src = Vec::new();
        let mut idx = Vec::new();
        for (fi, f) in faces.iter().enumerate() {
            if !gids.contains(&groups[fi]) {
                continue;
            }
            let c: Vec<u32> = (0..4)
                .map(|k| {
                    let key = (f[k], uvf[fi][k]);
                    *map.entry(key).or_insert_with(|| {
                        verts.push(pos[f[k] as usize].to_array());
                        let u = uvs[uvf[fi][k] as usize];
                        // glTF uvs run top-down.
                        puv.push([u[0], 1.0 - u[1]]);
                        src.push(f[k]);
                        (verts.len() - 1) as u32
                    })
                })
                .collect();
            // Angle-weighted corner normals of the quad itself (not of its
            // two triangles: one fixed diagonal per quad biases the
            // shading into faint streaks along the body).
            for k in 0..4 {
                let pc = pos[f[k] as usize];
                let a = pos[f[(k + 1) % 4] as usize] - pc;
                let b = pos[f[(k + 3) % 4] as usize] - pc;
                if let Some(n) = a.cross(b).try_normalize() {
                    *qacc.entry(f[k]).or_insert(Vec3::ZERO) += n * a.angle_between(b);
                }
            }
            // Split along the shorter diagonal.
            let p = |i: usize| Vec3::from(verts[c[i] as usize]);
            if p(0).distance_squared(p(2)) <= p(1).distance_squared(p(3)) {
                idx.extend([c[0], c[1], c[2], c[0], c[2], c[3]]);
            } else {
                idx.extend([c[1], c[2], c[3], c[1], c[3], c[0]]);
            }
        }
        // Helpers that are open shells (the teeth are bands) get each hole
        // capped by a fan round its centroid, so every part is closed.
        if want != ["body"] {
            let mut dir: HashMap<(u32, u32), u32> = HashMap::new();
            for t in idx.as_chunks::<3>().0 {
                for k in 0..3 {
                    let (a0, b0) = (src[t[k] as usize], src[t[(k + 1) % 3] as usize]);
                    *dir.entry((a0, b0)).or_default() += 1;
                }
            }
            // Open edges by template vertex: (a, b) with no (b, a).
            let mut next: HashMap<u32, u32> = HashMap::new();
            for &(a0, b0) in dir.keys() {
                if !dir.contains_key(&(b0, a0)) {
                    next.insert(a0, b0);
                }
            }
            let first: HashMap<u32, u32> = src
                .iter()
                .enumerate()
                .rev()
                .map(|(i, s)| (*s, i as u32))
                .collect();
            let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
            let mut starts: Vec<u32> = next.keys().copied().collect();
            starts.sort_unstable();
            for s0 in starts {
                if seen.contains(&s0) {
                    continue;
                }
                let mut ring = vec![s0];
                seen.insert(s0);
                let mut cur = s0;
                while let Some(&n) = next.get(&cur) {
                    if n == s0 || seen.contains(&n) || ring.len() > 10_000 {
                        break;
                    }
                    seen.insert(n);
                    ring.push(n);
                    cur = n;
                }
                if ring.len() < 3 {
                    continue;
                }
                let c = ring.iter().map(|&t| pos[t as usize]).sum::<Vec3>() / ring.len() as f32;
                let ci = verts.len() as u32;
                verts.push(c.to_array());
                puv.push(puv[first[&ring[0]] as usize]);
                // Its weights: the ring's first vertex's.
                src.push(ring[0]);
                for k in 0..ring.len() {
                    let (a0, b0) = (ring[k], ring[(k + 1) % ring.len()]);
                    idx.extend([first[&b0], first[&a0], ci]);
                }
            }
        }
        // Weld by template vertex for normals.
        let mut gmap: HashMap<u32, u32> = HashMap::new();
        let weld: Vec<u32> = src
            .iter()
            .map(|s| {
                let n = gmap.len() as u32;
                *gmap.entry(*s).or_insert(n)
            })
            .collect();
        let tri_n = normals(&verts, &idx, &weld, gmap.len());
        let nrm: Vec<[f32; 3]> = src
            .iter()
            .zip(&tri_n)
            .map(|(s, t)| {
                qacc.get(s)
                    .and_then(|n| n.try_normalize())
                    .map_or(*t, |n| n.to_array())
            })
            .collect();
        let (j, w): (Vec<[u16; 4]>, Vec<[f32; 4]>) = src
            .iter()
            .map(|&s| top4(weights[s as usize].clone()))
            .unzip();
        let m: Vec<(String, Vec<[f32; 3]>)> = morphs
            .iter()
            .zip(&deltas)
            .map(|(m, d)| {
                (
                    m.0.clone(),
                    src.iter().map(|&s| d[s as usize].to_array()).collect(),
                )
            })
            .collect();
        Part {
            name: pname.to_string(),
            verts,
            normals: nrm,
            uvs: puv,
            indices: idx,
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0; 3],
            visible: true,
            skin: Some(skin(j, w)),
            morphs: (!m.is_empty()).then(|| Arc::new(m)),
            ..Part::default()
        }
    };
    let mut body = make(name, &["body"]);
    body.joints = Some(joint_list.clone());
    // A built-in skin look; any `material` item on the body replaces it.
    builtin_look(&mut body, "human_skin", skin_graph([0.80, 0.62, 0.52]));
    let mut out = vec![body];
    for e in &extras {
        let pn = format!("{name}_{e}");
        match e.as_str() {
            "teeth" => {
                let mut p = make(&pn, &["helper-upper-teeth", "helper-lower-teeth"]);
                builtin_look(
                    &mut p,
                    "human_teeth",
                    mouth_graph(["#f1ece0", "#ddd2bc"], 0.25, 0.6),
                );
                out.push(p);
            }
            "tongue" => {
                let mut p = make(&pn, &["helper-tongue"]);
                builtin_look(
                    &mut p,
                    "human_tongue",
                    mouth_graph(["#b8525e", "#d27a82"], 0.45, 0.15),
                );
                out.push(p);
            }
            _ => {
                // Eyeballs: spheres fitted to the eye helpers (they follow
                // the morphs), each on its eye bone.
                let mut p = Part {
                    name: pn,
                    rotation: [0.0, 0.0, 0.0, 1.0],
                    scale: [1.0; 3],
                    visible: true,
                    ..Part::default()
                };
                let (mut js, mut ws) = (Vec::new(), Vec::new());
                let (rings, segs) = [(12, 16), (16, 24), (24, 32)][level_n];
                for (g, bone) in [("helper-l-eye", "eye_l"), ("helper-r-eye", "eye_r")] {
                    let Some(gi) = a.groups.iter().position(|x| x == g) else {
                        continue;
                    };
                    let mut vs: Vec<u16> = a
                        .faces
                        .iter()
                        .filter(|f| f.group as usize == gi)
                        .flat_map(|f| f.v)
                        .collect();
                    vs.sort_unstable();
                    vs.dedup();
                    if vs.is_empty() {
                        continue;
                    }
                    let at = |v: u16| ev.pos[v as usize] * fitted.scale + Vec3::Y * fitted.dy;
                    let c = vs.iter().map(|&v| at(v)).sum::<Vec3>() / vs.len() as f32;
                    let rad = vs.iter().map(|&v| at(v).distance(c)).sum::<f32>() / vs.len() as f32;
                    let (v, uv, idx) = eye_sphere(c, rad, rings, segs);
                    let base = p.verts.len() as u32;
                    let bi = bone_names.iter().position(|b| b == bone).unwrap_or(0) as u16;
                    for x in &v {
                        p.normals.push((*x - c).normalize_or(Vec3::Z).to_array());
                        js.push([bi, 0, 0, 0]);
                        ws.push([1.0, 0.0, 0.0, 0.0]);
                    }
                    p.verts.extend(v.iter().map(|x| x.to_array()));
                    p.uvs.extend(uv);
                    p.indices.extend(idx.iter().map(|i| i + base));
                }
                // The built-in eye look: white factors over the eye graph
                // (a `material` item on the part replaces it).
                p.material.base_color = [1.0; 4];
                p.material.roughness = 1.0;
                p.material.metalness = 0.0;
                p.material.pbr = Some(crate::material::PbrRef {
                    material: "human_eye".into(),
                    params: Default::default(),
                    scale: 1.0,
                    graph: Some(Arc::new(eye_graph(
                        def.eye_color.unwrap_or([0.36, 0.22, 0.12, 1.0]),
                        def.pupil.unwrap_or(1.0),
                    ))),
                });
                p.skin = Some(skin(js, ws));
                out.push(p);
            }
        }
    }
    if let Some(t) = def.tris
        && (t as usize) < out[0].tri_count() as usize
    {
        let b = &out[0];
        out[0] = decimated(b, t, def.bake)?;
    }
    Ok(out)
}

/// The body decimated to `tris`, its skin weights, joints and morphs moved
/// over from the nearest dense vertex.
fn decimated(src: &Part, tris: u32, bake: bool) -> Result<Part, String> {
    let d = crate::decimate::Decimate {
        tris: Some(tris),
        bake,
        ..crate::decimate::Decimate::default()
    };
    let mut out = crate::decimate::decimate(&src.name, src, &d)?;
    // Nearest source vertex by a uniform grid.
    let cell = 0.02f32;
    let key = |v: Vec3| {
        [
            (v.x / cell).floor() as i32,
            (v.y / cell).floor() as i32,
            (v.z / cell).floor() as i32,
        ]
    };
    let mut grid: HashMap<[i32; 3], Vec<u32>> = HashMap::new();
    for (i, v) in src.verts.iter().enumerate() {
        grid.entry(key(Vec3::from(*v))).or_default().push(i as u32);
    }
    let near: Vec<u32> = out
        .verts
        .iter()
        .map(|v| {
            let p = Vec3::from(*v);
            let k = key(p);
            let mut best = (f32::MAX, 0u32);
            for reach in 1..6 {
                for dx in -reach..=reach {
                    for dy in -reach..=reach {
                        for dz in -reach..=reach {
                            for &i in grid
                                .get(&[k[0] + dx, k[1] + dy, k[2] + dz])
                                .map_or(&[][..], |v| v.as_slice())
                            {
                                let d = Vec3::from(src.verts[i as usize]).distance_squared(p);
                                if d < best.0 {
                                    best = (d, i);
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
        })
        .collect();
    if let Some(s) = &src.skin {
        out.skin = Some(Arc::new(SkinWeights {
            bones: s.bones.clone(),
            joints: near.iter().map(|&i| s.joints[i as usize]).collect(),
            weights: near.iter().map(|&i| s.weights[i as usize]).collect(),
        }));
    }
    if let Some(m) = &src.morphs {
        out.morphs = Some(Arc::new(
            m.iter()
                .map(|(n, d)| (n.clone(), near.iter().map(|&i| d[i as usize]).collect()))
                .collect(),
        ));
    }
    out.joints = src.joints.clone();
    out.material = src.material.clone();
    if !src.uvs.is_empty() {
        out = with_source_uvs(out, src);
    }
    Ok(out)
}

/// The template's uvs on a decimated body. Each light triangle takes its
/// corners' uvs from the one dense triangle nearest its centroid (the
/// corners projected onto it, barycentric clamped to it), so a triangle
/// never mixes two uv islands; vertices are split where neighbouring
/// triangles disagree (the seams), carrying normal, weights and morphs.
fn with_source_uvs(mut out: Part, src: &Part) -> Part {
    let sv = |i: u32| Vec3::from(src.verts[i as usize]);
    let st = src.indices.as_chunks::<3>().0;
    let cell = 0.03f32;
    let key = |v: Vec3| {
        [
            (v.x / cell).floor() as i32,
            (v.y / cell).floor() as i32,
            (v.z / cell).floor() as i32,
        ]
    };
    let mut grid: HashMap<[i32; 3], Vec<u32>> = HashMap::new();
    for (ti, t) in st.iter().enumerate() {
        let c = (sv(t[0]) + sv(t[1]) + sv(t[2])) / 3.0;
        grid.entry(key(c)).or_default().push(ti as u32);
    }
    let closest = |p: Vec3, t: &[u32; 3]| -> (f32, [f32; 3]) {
        let (a, b, c) = (sv(t[0]), sv(t[1]), sv(t[2]));
        let n = (b - a).cross(c - a);
        let nn = n.length_squared().max(1e-20);
        let q = p - n * ((p - a).dot(n) / nn);
        let bc = |u: Vec3, v: Vec3| (u - q).cross(v - q).dot(n) / nn;
        let w = [bc(b, c), bc(c, a), bc(a, b)];
        let w = w.map(|x| x.max(0.0));
        let sum = (w[0] + w[1] + w[2]).max(1e-12);
        let w = w.map(|x| x / sum);
        let on = a * w[0] + b * w[1] + c * w[2];
        (on.distance_squared(p), w)
    };
    // uv islands of the dense part: triangles sharing a vertex (its
    // vertices are already split at the seams).
    let mut parent: Vec<u32> = (0..src.verts.len() as u32).collect();
    fn root(p: &mut [u32], mut x: u32) -> u32 {
        while p[x as usize] != x {
            p[x as usize] = p[p[x as usize] as usize];
            x = p[x as usize];
        }
        x
    }
    for t in st {
        let r0 = root(&mut parent, t[0]);
        for &v in &t[1..] {
            let r = root(&mut parent, v);
            parent[r as usize] = r0;
        }
    }
    let island: Vec<u32> = st.iter().map(|t| root(&mut parent, t[0])).collect();
    // The nearest dense triangle to `p` (of island `only`, if given).
    let nearest = |p: Vec3, only: Option<u32>| -> Option<(u32, [f32; 3])> {
        let k = key(p);
        let mut best: Option<(f32, u32, [f32; 3])> = None;
        let mut found_at = None;
        for reach in 0i32..10 {
            if found_at.is_some_and(|f| reach > f + 1) {
                break;
            }
            for dx in -reach..=reach {
                for dy in -reach..=reach {
                    for dz in -reach..=reach {
                        if dx.abs().max(dy.abs()).max(dz.abs()) != reach {
                            continue;
                        }
                        for &ti in grid
                            .get(&[k[0] + dx, k[1] + dy, k[2] + dz])
                            .map_or(&[][..], |v| v.as_slice())
                        {
                            if only.is_some_and(|o| island[ti as usize] != o) {
                                continue;
                            }
                            let (d, w) = closest(p, &st[ti as usize]);
                            if best.is_none_or(|b| d < b.0) {
                                best = Some((d, ti, w));
                            }
                        }
                    }
                }
            }
            if best.is_some() && found_at.is_none() {
                found_at = Some(reach);
            }
        }
        best.map(|b| (b.1, b.2))
    };
    let ov: Vec<Vec3> = out.verts.iter().map(|v| Vec3::from(*v)).collect();
    let mut verts = Vec::new();
    let mut normals = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut from: Vec<u32> = Vec::new();
    let mut map: HashMap<(u32, u32), u32> = HashMap::new();
    let mut idx = Vec::with_capacity(out.indices.len());
    for t in out.indices.as_chunks::<3>().0 {
        let c = (ov[t[0] as usize] + ov[t[1] as usize] + ov[t[2] as usize]) / 3.0;
        let Some((ti, _)) = nearest(c, None) else {
            idx.extend(t.iter().copied());
            continue;
        };
        let isl = island[ti as usize];
        for &v in t {
            // One vertex per (light vertex, island): the same closest point
            // for every triangle of that island, so they weld exactly.
            let i = *map.entry((v, isl)).or_insert_with(|| {
                let (sj, w) = nearest(ov[v as usize], Some(isl)).unwrap_or((ti, [1.0, 0.0, 0.0]));
                let s3 = &st[sj as usize];
                let mut uv = [0.0f32; 2];
                for k in 0..3 {
                    let u = src.uvs[s3[k] as usize];
                    uv[0] += u[0] * w[k];
                    uv[1] += u[1] * w[k];
                }
                verts.push(out.verts[v as usize]);
                normals.push(
                    out.normals
                        .get(v as usize)
                        .copied()
                        .unwrap_or([0.0, 1.0, 0.0]),
                );
                uvs.push(uv);
                from.push(v);
                (verts.len() - 1) as u32
            });
            idx.push(i);
        }
    }
    if let Some(sk) = &out.skin {
        out.skin = Some(Arc::new(SkinWeights {
            bones: sk.bones.clone(),
            joints: from.iter().map(|&i| sk.joints[i as usize]).collect(),
            weights: from.iter().map(|&i| sk.weights[i as usize]).collect(),
        }));
    }
    if let Some(m) = &out.morphs {
        out.morphs = Some(Arc::new(
            m.iter()
                .map(|(n, d)| (n.clone(), from.iter().map(|&i| d[i as usize]).collect()))
                .collect(),
        ));
    }
    out.verts = verts;
    out.normals = normals;
    out.uvs = uvs;
    out.indices = idx;
    out
}

/// The names a human item writes.
pub fn outputs(name: &str, def: &crate::body::BodyDef) -> Vec<String> {
    let mut v = vec![name.to_string()];
    match &def.parts {
        Some(p) => v.extend(p.iter().map(|e| format!("{name}_{e}"))),
        None => v.extend(EXTRAS.iter().map(|e| format!("{name}_{e}"))),
    }
    v
}

/// Checks a human item's fields (names against the template).
pub fn validate(def: &crate::body::BodyDef) -> Result<(), String> {
    let p = Params::of(def)?;
    if let Some(d) = def.detail
        && d > 2
    {
        return Err(format!("detail: 0, 1 or 2, got {d}"));
    }
    if let Some(ps) = &def.parts {
        for e in ps {
            if !EXTRAS.contains(&e.as_str()) {
                return Err(format!("parts: `{e}` is not one of {}", EXTRAS.join(", ")));
            }
        }
    }
    for w in &def.warps {
        if !crate::deform::LOCAL_MODES.contains(&w.mode.as_str()) {
            return Err(format!(
                "warps: `{}` is a field warp; a human takes local warps only ({})",
                w.mode,
                crate::deform::LOCAL_MODES.join(", ")
            ));
        }
    }
    let a = asset::load()?;
    let mut scratch = vec![Vec3::ZERO; a.verts.len()];
    add_details(&mut scratch, &a, &p.shape, &p.expression, p.ethnicity)?;
    for (k, s) in &def.shapes {
        add_details(&mut scratch, &a, &s.shape, &s.expression, p.ethnicity)
            .map_err(|e| format!("shapes.{k}: {e}"))?;
    }
    Ok(())
}

/// The detail key and expression unit names (for docs and errors).
pub fn key_names() -> Result<(Vec<String>, Vec<String>), String> {
    let a = asset::load()?;
    Ok((
        a.keys.iter().map(|k| k.name.clone()).collect(),
        a.units.iter().map(|u| u.name.clone()).collect(),
    ))
}

#[cfg(test)]
mod tests;
