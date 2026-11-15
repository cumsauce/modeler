//! Quality lints and relation checks: catch "messy" (near misses: faces a
//! few mm off flush, almost-symmetric models, uneven runs, sizes 1% apart),
//! "boxy" (mostly unrounded boxes) and "sparse" (big bare surfaces) models
//! numerically, plus the `verify` relation checks (`coaxial`, `flush`,
//! `centered_on`, `symmetric`, `equal_spacing`, `density`).

use crate::{Model, Op, Part, Vec3};

/// Visible parts the lints look at (no leaf cards, no empty meshes).
fn visible(model: &Model) -> Vec<(&Part, Vec3, Vec3)> {
    model
        .parts
        .iter()
        .filter(|p| p.visible && !p.verts.is_empty() && !p.cards)
        .map(|p| {
            let (lo, hi) = p.bounds();
            (&**p, lo, hi)
        })
        .collect()
}

fn ax(v: Vec3, k: usize) -> f32 {
    v[k]
}

const AXES: [&str; 3] = ["x", "y", "z"];
const MM: f32 = 0.001;
/// Most lines a lint family adds to the warnings.
const CAP: usize = 4;
/// Pairwise lints skip models with more visible parts than this.
const PAIR_LIMIT: usize = 400;

fn capped(mut lines: Vec<String>, what: &str) -> Vec<String> {
    if lines.len() > CAP {
        let more = lines.len() - CAP;
        lines.truncate(CAP);
        lines.push(format!("... and {more} more {what}"));
    }
    lines
}

/// World volume of a closed mesh (signed, outward = positive).
fn volume(p: &Part) -> f64 {
    let mut v = 0.0f64;
    for t in p.indices.as_chunks::<3>().0 {
        let a = p.world_vert(t[0] as usize);
        let b = p.world_vert(t[1] as usize);
        let c = p.world_vert(t[2] as usize);
        v += f64::from(a.dot(b.cross(c))) / 6.0;
    }
    v
}

/// An unrounded, axis-aligned box: at most 12 triangles filling its world
/// bounds.
pub fn is_plain_box(p: &Part, lo: Vec3, hi: Vec3) -> bool {
    if p.tri_count() != 12 {
        return false;
    }
    let d = hi - lo;
    let bv = f64::from(d.x) * f64::from(d.y) * f64::from(d.z);
    bv > 0.0 && (volume(p).abs() / bv) > 0.995
}

/// Every quality lint for `scene_summary` / `apply_ops` warnings.
pub fn lints(model: &Model) -> Vec<String> {
    let vis = visible(model);
    let mut out = Vec::new();
    if vis.len() >= 2 && vis.len() <= PAIR_LIMIT {
        out.extend(capped(near_flush(&vis), "near-flush faces"));
        out.extend(capped(near_sizes(&vis), "near-equal sizes"));
    }
    out.extend(near_symmetry(&vis));
    out.extend(capped(uneven_runs(model, &vis), "uneven runs"));
    out.extend(boxiness(&vis));
    out
}

/// Faces 1-5 mm off coplanar, or a 1-5 mm gap where two parts nearly touch.
fn near_flush(vis: &[(&Part, Vec3, Vec3)]) -> Vec<String> {
    let mut out = Vec::new();
    let band = |d: f32| (MM..=5.0 * MM).contains(&d.abs());
    for (i, &(a, alo, ahi)) in vis.iter().enumerate() {
        for &(b, blo, bhi) in &vis[i + 1..] {
            // Neighbours only: the boxes come within 5 mm on every axis.
            if (0..3)
                .any(|k| ax(blo, k) - ax(ahi, k) > 5.0 * MM || ax(alo, k) - ax(bhi, k) > 5.0 * MM)
            {
                continue;
            }
            let mut found = None;
            for (k, axis) in AXES.iter().enumerate() {
                let others = [(k + 1) % 3, (k + 2) % 3];
                let overlap = others
                    .iter()
                    .all(|&j| ax(alo, j) < ax(bhi, j) && ax(blo, j) < ax(ahi, j));
                // A gap: b just above a's max face (or the other way round).
                let gaps = [ax(blo, k) - ax(ahi, k), ax(alo, k) - ax(bhi, k)];
                if overlap && let Some(g) = gaps.iter().copied().find(|&g| g > 0.0 && band(g)) {
                    found = Some(format!(
                        "parts `{}` and `{}` nearly touch: a {:.1} mm gap along {} (close it so they meet, or open it to a clear gap; `place` drops parts exactly onto a surface)",
                        a.name,
                        b.name,
                        g / MM,
                        axis
                    ));
                    break;
                }
                let touching = gaps.iter().any(|g| g.abs() < 0.5 * MM);
                let thick = ax(ahi - alo, k) > 0.01 && ax(bhi - blo, k) > 0.01;
                if touching || !thick || !overlap {
                    continue;
                }
                for (name, d) in [
                    ("max", ax(ahi, k) - ax(bhi, k)),
                    ("min", ax(alo, k) - ax(blo, k)),
                ] {
                    if band(d) {
                        found = Some(format!(
                            "parts `{}` and `{}`: their {name} {} faces are {:.1} mm off flush (align them exactly, or offset clearly; verify flush checks it)",
                            a.name,
                            b.name,
                            axis,
                            d.abs() / MM
                        ));
                        break;
                    }
                }
                if found.is_some() {
                    break;
                }
            }
            out.extend(found);
        }
    }
    out
}

/// Pairs whose sizes differ by under 2% on every axis (but not exactly).
fn near_sizes(vis: &[(&Part, Vec3, Vec3)]) -> Vec<String> {
    let mut out = Vec::new();
    for (i, &(a, alo, ahi)) in vis.iter().enumerate() {
        let sa = ahi - alo;
        for &(b, blo, bhi) in &vis[i + 1..] {
            let sb = bhi - blo;
            let mut close = true;
            let mut worst = (0.0f32, 0usize);
            for k in 0..3 {
                let (x, y) = (ax(sa, k), ax(sb, k));
                let d = (x - y).abs();
                let m = x.max(y);
                if m < 0.01 || d > 0.02 * m {
                    close = false;
                    break;
                }
                if d > worst.0 {
                    worst = (d, k);
                }
            }
            let (d, k) = worst;
            let m = ax(sa, k).max(ax(sb, k));
            if close && d > 0.5 * MM && d > 0.002 * m {
                out.push(format!(
                    "parts `{}` and `{}` are almost the same size: {:.3} vs {:.3} m along {} ({:.1}% apart; make them equal, or clearly different)",
                    a.name,
                    b.name,
                    ax(sa, k),
                    ax(sb, k),
                    AXES[k],
                    100.0 * d / m
                ));
            }
        }
    }
    out
}

/// Per part, the smallest box error of its mirror image about `x = c`
/// against any part.
fn mirror_errors(vis: &[(&Part, Vec3, Vec3)], c: f32) -> Vec<f32> {
    vis.iter()
        .map(|&(_, lo, hi)| {
            let mlo = Vec3::new(2.0 * c - hi.x, lo.y, lo.z);
            let mhi = Vec3::new(2.0 * c - lo.x, hi.y, hi.z);
            vis.iter()
                .map(|&(_, blo, bhi)| {
                    (blo - mlo)
                        .abs()
                        .max_element()
                        .max((bhi - mhi).abs().max_element())
                })
                .fold(f32::MAX, f32::min)
        })
        .collect()
}

/// A model mirror-symmetric about x = 0 (or its centre plane) except for a
/// few parts a little off.
fn near_symmetry(vis: &[(&Part, Vec3, Vec3)]) -> Vec<String> {
    if vis.len() < 4 || vis.len() > PAIR_LIMIT {
        return Vec::new();
    }
    let lo = vis.iter().fold(f32::MAX, |m, v| m.min(v.1.x));
    let hi = vis.iter().fold(f32::MIN, |m, v| m.max(v.2.x));
    let mut planes = vec![0.0];
    let mid = 0.5 * (lo + hi);
    if mid.abs() > 0.5 * MM {
        planes.push(mid);
    }
    let size = (hi - lo).max(0.01);
    let near_max = (0.02 * size).clamp(0.003, 0.03);
    let mut best: Option<(f32, Vec<f32>, usize)> = None;
    for c in planes {
        let errs = mirror_errors(vis, c);
        let exact = errs.iter().filter(|&&e| e < 0.5 * MM).count();
        if best.as_ref().is_none_or(|b| exact > b.2) {
            best = Some((c, errs, exact));
        }
    }
    let Some((c, errs, exact)) = best else {
        return Vec::new();
    };
    let near: Vec<(usize, f32)> = errs
        .iter()
        .enumerate()
        .filter(|&(_, &e)| (0.5 * MM..near_max).contains(&e))
        .map(|(i, &e)| (i, e))
        .collect();
    let n = vis.len();
    if near.is_empty() || near.len() > 3 || (exact + near.len()) * 10 < n * 8 || exact * 2 < n {
        return Vec::new();
    }
    let list: Vec<String> = near
        .iter()
        .map(|&(i, e)| format!("`{}` ({:.1} mm)", vis[i].0.name, e / MM))
        .collect();
    vec![format!(
        "the model is mirror-symmetric about x = {c:.3} except {} off its mirror image: make it exact (a `mirror` op), or clearly asymmetric; verify symmetric checks it",
        list.join(", ")
    )]
}

/// Array-like name stem: `post_3` -> `post`, `post` -> `post`.
fn stem(name: &str) -> &str {
    match name.rsplit_once('_') {
        Some((base, num)) if !num.is_empty() && num.bytes().all(|b| b.is_ascii_digit()) => base,
        _ => name,
    }
}

/// Positions along a run: centres sorted along the axis of largest spread,
/// when they lie on a line. Returns (axis, sorted (name, t)) or None.
fn run_line(items: &[(&str, Vec3)]) -> Option<(usize, Vec<(String, f32)>)> {
    let lo = items.iter().fold(Vec3::splat(f32::MAX), |m, i| m.min(i.1));
    let hi = items.iter().fold(Vec3::splat(f32::MIN), |m, i| m.max(i.1));
    let spread = hi - lo;
    let k = (0..3).max_by(|&a, &b| ax(spread, a).total_cmp(&ax(spread, b)))?;
    let span = ax(spread, k);
    if span < 0.01 {
        return None;
    }
    let mut sorted: Vec<(&str, Vec3)> = items.to_vec();
    sorted.sort_by(|a, b| ax(a.1, k).total_cmp(&ax(b.1, k)));
    let (first, last) = (sorted.first()?.1, sorted.last()?.1);
    let dir = (last - first).normalize_or_zero();
    let off = sorted
        .iter()
        .map(|(_, c)| {
            let v = *c - first;
            (v - dir * v.dot(dir)).length()
        })
        .fold(0.0f32, f32::max);
    if off > 0.02 * span + MM {
        return None;
    }
    Some((
        k,
        sorted
            .iter()
            .map(|(n, c)| (n.to_string(), (*c - first).dot(dir)))
            .collect(),
    ))
}

/// Gap statistics of a sorted run: (mean, worst deviation, index of the
/// worst gap).
fn gap_stats(ts: &[(String, f32)]) -> Option<(f32, f32, usize)> {
    let gaps: Vec<f32> = ts.windows(2).map(|w| w[1].1 - w[0].1).collect();
    if gaps.is_empty() {
        return None;
    }
    let mean = gaps.iter().sum::<f32>() / gaps.len() as f32;
    let (i, dev) = gaps
        .iter()
        .enumerate()
        .map(|(i, g)| (i, (g - mean).abs()))
        .max_by(|a, b| a.1.total_cmp(&b.1))?;
    Some((mean, dev, i))
}

/// Runs of similar parts (array-like names, or the same size) whose
/// spacing is nearly but not quite even.
fn uneven_runs(model: &Model, vis: &[(&Part, Vec3, Vec3)]) -> Vec<String> {
    use std::collections::BTreeMap;
    let scattered = |s: &str| {
        model
            .ops
            .iter()
            .any(|op| matches!(op, Op::Scatter { .. }) && crate::graph::output(op) == s)
    };
    let mut groups: BTreeMap<String, Vec<(&str, Vec3)>> = BTreeMap::new();
    for &(p, lo, hi) in vis {
        let s = stem(&p.name);
        if s == p.name
            && !vis
                .iter()
                .any(|v| v.0.name != p.name && stem(&v.0.name) == s)
        {
            // No numbered siblings: group by size instead.
            let d = hi - lo;
            let key = format!(
                "size:{}:{}:{}",
                (d.x / MM).round() as i64,
                (d.y / MM).round() as i64,
                (d.z / MM).round() as i64
            );
            groups
                .entry(key)
                .or_default()
                .push((&p.name, 0.5 * (lo + hi)));
            continue;
        }
        if scattered(s) {
            continue;
        }
        groups
            .entry(format!("name:{s}"))
            .or_default()
            .push((&p.name, 0.5 * (lo + hi)));
    }
    let mut out = Vec::new();
    for (key, items) in groups {
        if items.len() < 3 {
            continue;
        }
        let Some((k, ts)) = run_line(&items) else {
            continue;
        };
        let Some((mean, dev, i)) = gap_stats(&ts) else {
            continue;
        };
        if mean > 0.0 && dev > MM.max(0.01 * mean) && dev < 0.25 * mean {
            let label = key.strip_prefix("name:").map_or_else(
                || format!("{} same-size parts", items.len()),
                |s| format!("`{s}*` ({} parts)", items.len()),
            );
            out.push(format!(
                "{label} run along {} with uneven spacing: mean {:.3} m, but `{}` -> `{}` is {:.3} m (use a linear_array / path_array, or verify equal_spacing)",
                AXES[k],
                mean,
                ts[i].0,
                ts[i + 1].0,
                ts[i + 1].1 - ts[i].1
            ));
        }
    }
    out
}

fn boxiness(vis: &[(&Part, Vec3, Vec3)]) -> Vec<String> {
    let boxes = vis
        .iter()
        .filter(|&&(p, lo, hi)| is_plain_box(p, lo, hi))
        .count();
    if boxes >= 5 && boxes * 10 > vis.len() * 7 {
        vec![format!(
            "boxy: {boxes} of {} visible parts are unrounded axis-aligned boxes: round edges (`round` fillet / chamfer, or `smooth` subdiv), shape them (`loft`, `extrude` with scale_top, `deform` taper / bend) or vary their angles",
            vis.len()
        )]
    } else {
        Vec::new()
    }
}

// ---------------------------------------------------------------- verify

/// `[a, b]` or `{"pair": [a, b], "tol": 0.001, "axis": "y"}`.
#[derive(Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum PairSpec {
    Pair([String; 2]),
    With {
        pair: [String; 2],
        /// Meters (default 0.001).
        #[serde(default)]
        tol: Option<f32>,
        /// coaxial only: the shared axis (default y; the other two
        /// centre coordinates must match).
        #[serde(default)]
        axis: Option<String>,
    },
}

impl PairSpec {
    fn parts(&self) -> (&str, &str, f32, Option<&str>) {
        match self {
            PairSpec::Pair([a, b]) => (a, b, MM, None),
            PairSpec::With {
                pair: [a, b],
                tol,
                axis,
            } => (a, b, tol.unwrap_or(MM), axis.as_deref()),
        }
    }
}

/// `{"a": "lid", "b": "box", "face": "top", "tol"?}`: the named face of `a`
/// lies in the same plane as that face of `b`.
#[derive(Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FlushSpec {
    pub a: String,
    pub b: String,
    /// top / bottom / left / right / front / back.
    pub face: String,
    #[serde(default)]
    pub tol: Option<f32>,
}

/// Part names: one pattern (`"post_*"`) or a list.
#[derive(Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum Names {
    One(String),
    Many(Vec<String>),
}

impl Names {
    fn list(&self) -> Vec<String> {
        match self {
            Names::One(s) => vec![s.clone()],
            Names::Many(v) => v.clone(),
        }
    }
}

/// `{"parts": [...], "axis": "x", "center"?: 0, "tol"?: 0.001}`.
#[derive(Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SymmetricSpec {
    #[serde(default)]
    pub parts: Option<Names>,
    /// The mirror plane's normal axis (default x).
    #[serde(default)]
    pub axis: Option<String>,
    /// The plane's position on that axis (default 0).
    #[serde(default)]
    pub center: Option<f32>,
    #[serde(default)]
    pub tol: Option<f32>,
}

/// `{"parts": "post_*", "tol"?: 0.001}`.
#[derive(Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpacingSpec {
    pub parts: Names,
    #[serde(default)]
    pub tol: Option<f32>,
}

/// `{"min_features_per_m2"?: 0.25, "min_area"?: 4}`.
#[derive(Clone, Debug, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DensitySpec {
    #[serde(default)]
    pub min_features_per_m2: Option<f32>,
    #[serde(default)]
    pub min_area: Option<f32>,
}

fn axis_index(a: &str) -> Result<usize, String> {
    match a {
        "x" => Ok(0),
        "y" => Ok(1),
        "z" => Ok(2),
        o => Err(format!("axis `{o}`: use x, y or z")),
    }
}

/// The world box of the visible parts matching `pat` (`*` globs).
fn box_of(model: &Model, pat: &str) -> Result<(Vec3, Vec3), String> {
    let mut lo = Vec3::splat(f32::MAX);
    let mut hi = Vec3::splat(f32::MIN);
    let mut any = false;
    for p in model
        .parts
        .iter()
        .filter(|p| p.visible && !p.verts.is_empty())
    {
        if crate::name_matches(pat, &p.name) {
            let (a, b) = p.bounds();
            lo = lo.min(a);
            hi = hi.max(b);
            any = true;
        }
    }
    if any {
        Ok((lo, hi))
    } else {
        Err(format!("no visible part matches `{pat}`"))
    }
}

fn v3(v: Vec3) -> String {
    format!("[{:.4}, {:.4}, {:.4}]", v.x, v.y, v.z)
}

/// `coaxial`: the two parts' centres agree on the two axes across `axis`.
pub fn coaxial(model: &Model, p: &PairSpec) -> Result<String, String> {
    let (a, b, tol, axis) = p.parts();
    let k = axis_index(axis.unwrap_or("y"))?;
    let (alo, ahi) = box_of(model, a)?;
    let (blo, bhi) = box_of(model, b)?;
    let (ca, cb) = (0.5 * (alo + ahi), 0.5 * (blo + bhi));
    let others = [(k + 1) % 3, (k + 2) % 3];
    let off: Vec<String> = others
        .iter()
        .map(|&j| format!("{} {:+.4}", AXES[j], ax(cb, j) - ax(ca, j)))
        .collect();
    let worst = others
        .iter()
        .map(|&j| (ax(cb, j) - ax(ca, j)).abs())
        .fold(0.0f32, f32::max);
    let txt = format!(
        "`{a}` and `{b}` along {}: centre offset {} m (tol {tol})",
        AXES[k],
        off.join(", ")
    );
    if worst <= tol { Ok(txt) } else { Err(txt) }
}

/// `centered_on`: `a`'s footprint centre (x, z) is `b`'s.
pub fn centered_on(model: &Model, p: &PairSpec) -> Result<String, String> {
    let (a, b, tol, _) = p.parts();
    let (alo, ahi) = box_of(model, a)?;
    let (blo, bhi) = box_of(model, b)?;
    let (ca, cb) = (0.5 * (alo + ahi), 0.5 * (blo + bhi));
    let (dx, dz) = (ca.x - cb.x, ca.z - cb.z);
    let rel = if alo.y >= bhi.y - tol {
        format!(", {:.4} m above its top", alo.y - bhi.y)
    } else {
        String::new()
    };
    let txt = format!("`{a}` on `{b}`: centre off by x {dx:+.4}, z {dz:+.4} m (tol {tol}){rel}");
    if dx.abs() <= tol && dz.abs() <= tol {
        Ok(txt)
    } else {
        Err(txt)
    }
}

/// `flush`: the named face of `a` is in the plane of that face of `b`.
pub fn flush(model: &Model, f: &FlushSpec) -> Result<String, String> {
    let tol = f.tol.unwrap_or(MM);
    let (k, max) = match f.face.as_str() {
        "top" => (1, true),
        "bottom" => (1, false),
        "right" => (0, true),
        "left" => (0, false),
        "front" => (2, true),
        "back" => (2, false),
        o => return Err(format!("face `{o}`: use top/bottom/left/right/front/back")),
    };
    let (alo, ahi) = box_of(model, &f.a)?;
    let (blo, bhi) = box_of(model, &f.b)?;
    let (pa, pb) = if max {
        (ax(ahi, k), ax(bhi, k))
    } else {
        (ax(alo, k), ax(blo, k))
    };
    let txt = format!(
        "`{}` {} at {} = {pa:.4}, `{}` at {pb:.4}: {:.2} mm apart (tol {:.2} mm)",
        f.a,
        f.face,
        AXES[k],
        f.b,
        (pa - pb).abs() / MM,
        tol / MM
    );
    if (pa - pb).abs() <= tol {
        Ok(txt)
    } else {
        Err(txt)
    }
}

/// `symmetric`: every matched part has a mirror twin (or is its own) about
/// the plane `axis = center`.
pub fn symmetric(model: &Model, s: &SymmetricSpec) -> Result<String, String> {
    let k = axis_index(s.axis.as_deref().unwrap_or("x"))?;
    let c = s.center.unwrap_or(0.0);
    let tol = s.tol.unwrap_or(MM);
    let pats = s
        .parts
        .as_ref()
        .map_or_else(|| vec!["*".to_string()], Names::list);
    let boxes: Vec<(&str, Vec3, Vec3)> = model
        .parts
        .iter()
        .filter(|p| p.visible && !p.verts.is_empty())
        .filter(|p| pats.iter().any(|pat| crate::name_matches(pat, &p.name)))
        .map(|p| {
            let (lo, hi) = p.bounds();
            (p.name.as_str(), lo, hi)
        })
        .collect();
    if boxes.is_empty() {
        return Err(format!("no visible part matches {}", pats.join(", ")));
    }
    let mut bad = Vec::new();
    let mut worst = 0.0f32;
    for &(n, lo, hi) in &boxes {
        let (mut mlo, mut mhi) = (lo, hi);
        mlo[k] = 2.0 * c - ax(hi, k);
        mhi[k] = 2.0 * c - ax(lo, k);
        let (e, twin) = boxes
            .iter()
            .map(|&(m, blo, bhi)| {
                (
                    (blo - mlo)
                        .abs()
                        .max_element()
                        .max((bhi - mhi).abs().max_element()),
                    m,
                )
            })
            .fold((f32::MAX, ""), |a, b| if b.0 < a.0 { b } else { a });
        worst = worst.max(e);
        if e > tol {
            bad.push(format!(
                "`{n}` (nearest mirror `{twin}` {:.2} mm off)",
                e / MM
            ));
        }
    }
    let head = format!(
        "{} part(s) about {} = {c}: worst {:.2} mm (tol {:.2} mm)",
        boxes.len(),
        AXES[k],
        worst / MM,
        tol / MM
    );
    if bad.is_empty() {
        Ok(head)
    } else {
        Err(format!("{head}; without a mirror twin: {}", bad.join(", ")))
    }
}

/// `equal_spacing`: the matched parts' centres lie on a line with equal
/// gaps.
pub fn equal_spacing(model: &Model, s: &SpacingSpec) -> Result<String, String> {
    let tol = s.tol.unwrap_or(MM);
    let pats = s.parts.list();
    let items: Vec<(&str, Vec3)> = model
        .parts
        .iter()
        .filter(|p| p.visible && !p.verts.is_empty())
        .filter(|p| pats.iter().any(|pat| crate::name_matches(pat, &p.name)))
        .map(|p| {
            let (lo, hi) = p.bounds();
            (p.name.as_str(), 0.5 * (lo + hi))
        })
        .collect();
    if items.len() < 3 {
        return Err(format!(
            "{} part(s) match {}: need at least 3",
            items.len(),
            pats.join(", ")
        ));
    }
    let Some((k, ts)) = run_line(&items) else {
        return Err(format!(
            "the {} parts' centres are not on a line (equal_spacing checks straight runs)",
            items.len()
        ));
    };
    let Some((mean, dev, i)) = gap_stats(&ts) else {
        return Err("no gaps to measure".into());
    };
    let txt = format!(
        "{} parts along {}: mean gap {mean:.4} m, worst `{}` -> `{}` {:.4} m ({:+.2} mm; tol {:.2} mm)",
        items.len(),
        AXES[k],
        ts[i].0,
        ts[i + 1].0,
        ts[i + 1].1 - ts[i].1,
        (ts[i + 1].1 - ts[i].1 - mean) / MM,
        tol / MM
    );
    if dev <= tol { Ok(txt) } else { Err(txt) }
}

/// A flat region of one part: coplanar triangles.
struct Region {
    area: f32,
    normal: Vec3,
    lo: Vec3,
    hi: Vec3,
}

fn regions(p: &Part) -> Vec<Region> {
    use std::collections::HashMap;
    let mut map: HashMap<[i64; 4], Region> = HashMap::new();
    for t in p.indices.as_chunks::<3>().0 {
        let a = p.world_vert(t[0] as usize);
        let b = p.world_vert(t[1] as usize);
        let c = p.world_vert(t[2] as usize);
        let cr = (b - a).cross(c - a);
        let area = 0.5 * cr.length();
        if area <= 0.0 {
            continue;
        }
        let n = cr / (2.0 * area);
        let key = [
            (n.x * 50.0).round() as i64,
            (n.y * 50.0).round() as i64,
            (n.z * 50.0).round() as i64,
            (n.dot(a) / 0.005).round() as i64,
        ];
        let r = map.entry(key).or_insert(Region {
            area: 0.0,
            normal: n,
            lo: Vec3::splat(f32::MAX),
            hi: Vec3::splat(f32::MIN),
        });
        r.area += area;
        r.lo = r.lo.min(a).min(b).min(c);
        r.hi = r.hi.max(a).max(b).max(c);
    }
    map.into_values().collect()
}

fn boxes_meet(alo: Vec3, ahi: Vec3, blo: Vec3, bhi: Vec3, pad: f32) -> bool {
    (0..3).all(|k| ax(alo, k) <= ax(bhi, k) + pad && ax(blo, k) <= ax(ahi, k) + pad)
}

/// `density`: big flat surfaces with nothing on them.
pub fn density(model: &Model, d: &DensitySpec) -> Result<String, String> {
    let min_area = d.min_area.unwrap_or(4.0);
    let per_m2 = d.min_features_per_m2.unwrap_or(0.25);
    let vis = visible(model);
    let decals = crate::decal::collect(model).unwrap_or_default();
    let mut checked = 0;
    let mut bad = Vec::new();
    for &(p, _, _) in &vis {
        let regs = regions(p);
        // Undersides are rarely seen: skip them.
        for r in regs
            .iter()
            .filter(|r| r.area >= min_area && r.normal.y > -0.5)
        {
            checked += 1;
            // Other parts touching or within the surface.
            let mut features = vis
                .iter()
                .filter(|v| v.0.name != p.name && boxes_meet(r.lo, r.hi, v.1, v.2, 0.02))
                .count();
            // Silhouette breaks: the part's own other faces inside it
            // (insets, cut-outs, relief), not its border.
            let shrink = Vec3::splat(0.01);
            let (ilo, ihi) = (r.lo + shrink, r.hi - shrink);
            features += regs
                .iter()
                .filter(|o| !std::ptr::eq(*o, r))
                .filter(|o| {
                    let c = 0.5 * (o.lo + o.hi);
                    (0..3).all(|k| {
                        let flat = ax(r.hi - r.lo, k) < 0.03;
                        flat || (ax(ilo, k) < ax(c, k) && ax(c, k) < ax(ihi, k))
                    }) && o.area > 1e-4
                })
                .count()
                .min(20);
            features += decals
                .iter()
                .filter(|dc| {
                    let h = Vec3::splat(0.5 * dc.size.max_element());
                    boxes_meet(r.lo, r.hi, dc.position - h, dc.position + h, 0.0)
                })
                .count();
            let need = per_m2 * r.area;
            if (features as f32) < need {
                let tex = if p.material.has_pbr() {
                    "a pbr material"
                } else {
                    "a flat colour"
                };
                bad.push((
                    r.area,
                    format!(
                        "`{}` {:.1} m² face around {} has {features} feature(s), wants {:.0} ({tex})",
                        p.name,
                        r.area,
                        v3(0.5 * (r.lo + r.hi)),
                        need.ceil()
                    ),
                ));
            }
        }
    }
    bad.sort_by(|a, b| b.0.total_cmp(&a.0));
    if bad.is_empty() {
        Ok(format!(
            "{checked} flat surface(s) over {min_area} m², each with at least {per_m2} feature(s) per m²"
        ))
    } else {
        let n = bad.len();
        let list: Vec<String> = bad.into_iter().take(6).map(|b| b.1).collect();
        Err(format!(
            "{n} sparse surface(s): {}{} (break them up: scatter props or rocks onto them, panel or inset them with booleans, add decals, or macro variation in the material: noise / facing / gradient layers)",
            list.join("; "),
            if n > 6 { "; ..." } else { "" }
        ))
    }
}

#[cfg(test)]
mod tests {
    use crate::commands::{Session, VerifySpec};

    fn run(s: &mut Session, items: serde_json::Value) {
        let serde_json::Value::Array(items) = items else {
            panic!("array");
        };
        s.apply_values_as(None, items, true)
            .unwrap_or_else(|e| panic!("{e}"));
    }

    fn check(s: &Session, v: serde_json::Value, name: &str) -> (bool, String) {
        let spec: VerifySpec = serde_json::from_value(v).unwrap_or_else(|e| panic!("{e}"));
        let c = s
            .verify_checks(&spec)
            .into_iter()
            .find(|c| c.0.starts_with(name))
            .unwrap_or_else(|| panic!("no {name} check"));
        (c.1, c.2)
    }

    fn bx(name: &str, size: [f32; 3], at: [f32; 3]) -> serde_json::Value {
        serde_json::json!({"type": "primitive", "name": name, "kind": "box", "size": size, "translation": at})
    }

    #[test]
    fn near_flush_and_near_size_lints_fire() {
        let mut s = Session::default();
        run(
            &mut s,
            serde_json::json!([
                bx("base", [1.0, 0.5, 1.0], [0.0, 0.25, 0.0]),
                // 3 mm gap above the base.
                bx("lid", [0.6, 0.1, 0.6], [0.0, 0.553, 0.0]),
                bx("leg_a", [0.1, 0.4, 0.1], [3.0, 0.2, 0.0]),
                bx("leg_b", [0.101, 0.4, 0.1], [4.0, 0.2, 0.0])
            ]),
        );
        let l = s.lints();
        assert!(
            l.iter()
                .any(|w| w.contains("nearly touch") && w.contains("`lid`")),
            "{l:?}"
        );
        assert!(
            l.iter()
                .any(|w| w.contains("almost the same size") && w.contains("leg_b")),
            "{l:?}"
        );
    }

    #[test]
    fn exact_contacts_do_not_lint() {
        let mut s = Session::default();
        run(
            &mut s,
            serde_json::json!([
                bx("base", [1.0, 0.5, 1.0], [0.0, 0.25, 0.0]),
                bx("lid", [0.6, 0.1, 0.6], [0.0, 0.55, 0.0])
            ]),
        );
        let l = s.lints();
        assert!(
            !l.iter()
                .any(|w| w.contains("nearly") || w.contains("flush")),
            "{l:?}"
        );
    }

    #[test]
    fn near_symmetry_uneven_runs_and_boxiness() {
        let mut s = Session::default();
        run(
            &mut s,
            serde_json::json!([
                bx("body", [1.0, 0.5, 0.5], [0.0, 0.25, 0.0]),
                bx("arm_l", [0.2, 0.2, 0.2], [-0.6, 0.3, 0.0]),
                bx("arm_r", [0.2, 0.2, 0.2], [0.605, 0.3, 0.0]),
                bx("ear_l", [0.1, 0.1, 0.1], [-0.3, 0.55, 0.0]),
                bx("ear_r", [0.1, 0.1, 0.1], [0.3, 0.55, 0.0]),
                bx("post", [0.05, 0.5, 0.05], [0.0, 0.25, 2.0]),
                bx("post_1", [0.05, 0.5, 0.05], [0.0, 0.25, 3.0]),
                bx("post_2", [0.05, 0.5, 0.05], [0.0, 0.25, 4.1]),
                bx("post_3", [0.05, 0.5, 0.05], [0.0, 0.25, 5.0])
            ]),
        );
        let l = s.lints();
        assert!(
            l.iter()
                .any(|w| w.contains("mirror-symmetric") && w.contains("arm_r")),
            "{l:?}"
        );
        assert!(
            l.iter()
                .any(|w| w.contains("uneven spacing") && w.contains("post")),
            "{l:?}"
        );
        assert!(l.iter().any(|w| w.starts_with("boxy")), "{l:?}");
        let (ok, t) = check(
            &s,
            serde_json::json!({"equal_spacing": {"parts": "post*"}}),
            "equal_spacing",
        );
        assert!(!ok && t.contains("post_3"), "{t}");
        let (ok, t) = check(
            &s,
            serde_json::json!({"symmetric": {"parts": ["arm*", "ear*", "body"]}}),
            "symmetric",
        );
        assert!(!ok && t.contains("arm"), "{t}");
        let (ok, _) = check(
            &s,
            serde_json::json!({"symmetric": {"parts": ["ear*", "body"], "axis": "x"}}),
            "symmetric",
        );
        assert!(ok);
    }

    #[test]
    fn relation_checks_report_offsets() {
        let mut s = Session::default();
        run(
            &mut s,
            serde_json::json!([
                bx("table", [1.0, 0.75, 1.0], [0.0, 0.375, 0.0]),
                {"type": "primitive", "name": "vase", "kind": "cylinder", "size": [0.05, 0.2], "translation": [0.0, 0.85, 0.0]},
                bx("mug", [0.08, 0.1, 0.08], [0.2, 0.8, 0.0]),
                bx("trim", [1.0, 0.02, 0.02], [0.0, 0.745, 0.51])
            ]),
        );
        assert!(
            check(
                &s,
                serde_json::json!({"coaxial": [["vase", "table"]]}),
                "coaxial"
            )
            .0
        );
        let (ok, t) = check(
            &s,
            serde_json::json!({"centered_on": [["mug", "table"]]}),
            "centered_on",
        );
        assert!(!ok && t.contains("+0.2"), "{t}");
        let (ok, t) = check(
            &s,
            serde_json::json!({"centered_on": [{"pair": ["mug", "table"], "tol": 0.3}]}),
            "centered_on",
        );
        assert!(ok, "{t}");
        let (ok, t) = check(
            &s,
            serde_json::json!({"flush": [{"a": "trim", "b": "table", "face": "top"}]}),
            "flush",
        );
        assert!(!ok && t.contains("mm apart"), "{t}");
        let (ok, json) = {
            let spec: VerifySpec =
                serde_json::from_value(serde_json::json!({"flush": [{"a": "trim", "b": "table", "face": "top", "tol": 0.01}]}))
                    .unwrap_or_else(|e| panic!("{e}"));
            let c = s.verify_checks(&spec);
            (c.iter().all(|c| c.1), Session::checks_json(&c))
        };
        assert!(ok && json[0]["pass"] == true, "{json}");
    }

    #[test]
    fn density_flags_a_bare_floor() {
        let mut s = Session::default();
        run(
            &mut s,
            serde_json::json!([bx("floor", [6.0, 0.1, 6.0], [0.0, 0.05, 0.0])]),
        );
        let (ok, t) = check(&s, serde_json::json!({"density": {}}), "density");
        assert!(!ok && t.contains("`floor`") && t.contains("scatter"), "{t}");
        let mut items = Vec::new();
        for i in 0..10 {
            items.push(bx(
                &format!("crate_{i}"),
                [0.4, 0.4, 0.4],
                [-2.5 + 0.55 * i as f32, 0.3, 0.0],
            ));
        }
        run(&mut s, serde_json::Value::Array(items));
        let (ok, t) = check(&s, serde_json::json!({"density": {}}), "density");
        assert!(ok, "{t}");
    }
}
