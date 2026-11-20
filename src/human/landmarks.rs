//! Landmarks and zones of the human template.
//!
//! Every landmark is a template vertex (or a loop of them), found once from
//! the data: the eye-closure expression units move the upper lids (their
//! most-moved row is the lid margin, and where it lands closed is the lower
//! margin), the mouth-corner-puller unit finds the mouth corners, the ear,
//! nipple and navel detail targets find those, and the midline profile
//! finds the nose, lips and chin. Because they are vertex indices they
//! follow every shape parameter, expression and pose: a landmark's
//! position is that vertex's evaluated position.
//!
//! Zones are per-template-vertex weights (0..1): `lips`, `face`, `scalp`,
//! `brows`, `nails`, `areola`, `hands`, `feet`, `ears`, `nose`, `cheeks`,
//! `knees`, `elbows`, `knuckles`, `tzone`. The material node `zone`
//! rasterizes one over the template's uv layout.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use bevy::math::Vec3;

use super::asset::{self, HumanAsset};

/// Every zone name, in documentation order.
pub const ZONES: &[&str] = &[
    "lips", "face", "scalp", "brows", "nails", "areola", "hands", "feet", "ears", "nose", "cheeks",
    "knees", "elbows", "knuckles", "tzone",
];

/// The landmarks of the template.
#[derive(Debug, Default)]
pub struct Landmarks {
    /// Named points: (name, template vertex).
    pub points: Vec<(String, u32)>,
    /// Named ordered loops / curves: (name, template vertices).
    pub loops: Vec<(String, Vec<u32>)>,
    /// Zone weights per template vertex.
    pub zones: Vec<(String, Vec<f32>)>,
}

impl Landmarks {
    pub fn point(&self, n: &str) -> Option<u32> {
        self.points.iter().find(|p| p.0 == n).map(|p| p.1)
    }
    pub fn curve(&self, n: &str) -> Option<&[u32]> {
        self.loops.iter().find(|p| p.0 == n).map(|p| p.1.as_slice())
    }
    pub fn zone(&self, n: &str) -> Option<&[f32]> {
        self.zones.iter().find(|p| p.0 == n).map(|p| p.1.as_slice())
    }
    /// Landmark points at evaluated positions (anchors on the body part).
    pub fn positions(&self, pos: &[Vec3]) -> Vec<(String, [f32; 3])> {
        self.points
            .iter()
            .filter_map(|(n, v)| pos.get(*v as usize).map(|p| (n.clone(), p.to_array())))
            .collect()
    }
}

static LM: OnceLock<Result<Arc<Landmarks>, String>> = OnceLock::new();

/// The template's landmarks (computed once).
pub fn get() -> Result<Arc<Landmarks>, String> {
    LM.get_or_init(|| asset::load().map(|a| Arc::new(compute(&a))))
        .clone()
}

fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Per-vertex magnitudes of a target.
fn mags(a: &HumanAsset, ti: usize) -> Vec<f32> {
    let mut m = vec![0.0f32; a.verts.len()];
    let t = &a.targets[ti];
    for (&v, d) in t.idx.iter().zip(&t.d) {
        m[v as usize] = Vec3::new(d[0] as f32, d[1] as f32, d[2] as f32).length() * asset::QUANTUM;
    }
    m
}

fn deltas(a: &HumanAsset, ti: usize) -> HashMap<u32, Vec3> {
    let t = &a.targets[ti];
    t.idx
        .iter()
        .zip(&t.d)
        .map(|(&v, d)| {
            (
                v as u32,
                Vec3::new(d[0] as f32, d[1] as f32, d[2] as f32) * asset::QUANTUM,
            )
        })
        .collect()
}

fn unit_target(a: &HumanAsset, name: &str) -> Option<usize> {
    let u = a.units.iter().find(|u| u.name == name)?;
    [2usize, 0, 1]
        .iter()
        .map(|&e| u.by_ethnicity[e])
        .find(|&t| t >= 0)
        .map(|t| t as usize)
}

fn key_target(a: &HumanAsset, name: &str) -> Option<usize> {
    let k = a.keys.iter().find(|k| k.name == name)?;
    [k.high, k.low]
        .into_iter()
        .find(|&t| t >= 0)
        .map(|t| t as usize)
}

fn compute(a: &HumanAsset) -> Landmarks {
    let n = a.verts.len();
    let p: Vec<Vec3> = a.verts.iter().map(|v| Vec3::from(*v)).collect();
    let mut on_body = vec![false; n];
    let mut nrm = vec![Vec3::ZERO; n];
    for f in a.faces.iter().filter(|f| f.group == 0) {
        for k in 0..4 {
            let c = f.v[k] as usize;
            on_body[c] = true;
            let e1 = p[f.v[(k + 1) % 4] as usize] - p[c];
            let e2 = p[f.v[(k + 3) % 4] as usize] - p[c];
            nrm[c] += e1.cross(e2);
        }
    }
    let nrm: Vec<Vec3> = nrm.iter().map(|v| v.normalize_or(Vec3::Y)).collect();
    let body: Vec<u32> = (0..n as u32).filter(|&i| on_body[i as usize]).collect();
    let joint = |name: &str| -> Option<Vec3> {
        let j = a.joints.iter().find(|j| j.0 == name)?;
        if j.1.is_empty() {
            return None;
        }
        Some(j.1.iter().map(|&v| p[v as usize]).sum::<Vec3>() / j.1.len() as f32)
    };
    let nearest = |q: Vec3, filt: &dyn Fn(u32) -> bool| -> u32 {
        body.iter()
            .copied()
            .filter(|&i| filt(i))
            .min_by(|&x, &y| {
                p[x as usize]
                    .distance_squared(q)
                    .total_cmp(&p[y as usize].distance_squared(q))
            })
            .unwrap_or(0)
    };
    // The frontmost body vertex near (x, y): skin seen from the front.
    let front_at = |x: f32, y: f32, r: f32| -> u32 {
        body.iter()
            .copied()
            .filter(|&i| {
                let v = p[i as usize];
                (v.x - x).abs() < r && (v.y - y).abs() < r && nrm[i as usize].z > 0.2
            })
            // The front surface (within 1 cm of the frontmost), then the
            // vertex nearest (x, y) on it.
            .map(|i| (i, p[i as usize]))
            .collect::<Vec<_>>()
            .pipe_front(x, y)
            .unwrap_or_else(|| nearest(Vec3::new(x, y, 1.0), &|_| true))
    };
    let mut lm = Landmarks::default();
    let side = |x: f32| if x >= 0.0 { "l" } else { "r" };
    let head = joint("head____head").unwrap_or(Vec3::new(0.0, 1.55, 0.0));
    let crown = body
        .iter()
        .copied()
        .max_by(|&i, &j| p[i as usize].y.total_cmp(&p[j as usize].y))
        .unwrap_or(0);
    lm.points.push(("crown".into(), crown));

    // ---- eyes: the closure units.
    let mut eye_y = head.y + 0.03;
    let mut corners: Vec<(f32, Vec3, Vec3)> = Vec::new(); // (x sign, inner, outer)
    let mut uppers: Vec<Vec<u32>> = Vec::new();
    for u in ["eye-left-closure", "eye-right-closure"] {
        let Some(ti) = unit_target(a, u) else {
            continue;
        };
        let m = mags(a, ti);
        let d = deltas(a, ti);
        let mx = body.iter().map(|&i| m[i as usize]).fold(0.0, f32::max);
        if mx <= 0.0 {
            continue;
        }
        let cand: Vec<u32> = body
            .iter()
            .copied()
            .filter(|&i| m[i as usize] > 0.25 * mx)
            .collect();
        if cand.len() < 3 {
            continue;
        }
        let (lo, hi) = cand.iter().fold((f32::MAX, f32::MIN), |(l, h), &i| {
            (l.min(p[i as usize].x), h.max(p[i as usize].x))
        });
        let sx = (lo + hi) * 0.5;
        // Inner first: the end nearer the midline.
        let bins = 11usize;
        let mut upper: Vec<u32> = Vec::new();
        for b in 0..bins {
            let t = b as f32 / (bins - 1) as f32;
            let tx = if sx >= 0.0 {
                lo + (hi - lo) * t
            } else {
                hi - (hi - lo) * t
            };
            let w = (hi - lo) / (bins as f32) * 0.75;
            if let Some(v) = cand
                .iter()
                .copied()
                .filter(|&i| (p[i as usize].x - tx).abs() <= w)
                .max_by(|&i, &j| m[i as usize].total_cmp(&m[j as usize]))
                && upper.last() != Some(&v)
            {
                upper.push(v);
            }
        }
        // The lower margin: where the closed upper margin lands.
        let mut lower: Vec<u32> = Vec::new();
        for &u0 in &upper {
            let q = p[u0 as usize] + d.get(&u0).copied().unwrap_or(Vec3::ZERO);
            let v = nearest(q, &|i| m[i as usize] < 0.3 * mx);
            if lower.last() != Some(&v) {
                lower.push(v);
            }
        }
        let s = side(sx);
        let (ci, co) = (upper[0], upper[upper.len() - 1]);
        lm.points.push((format!("eye_corner_in_{s}"), ci));
        lm.points.push((format!("eye_corner_out_{s}"), co));
        let mid = upper[upper.len() / 2];
        lm.points.push((format!("lid_upper_{s}"), mid));
        lm.points
            .push((format!("lid_lower_{s}"), lower[lower.len() / 2]));
        eye_y = p[mid as usize].y;
        corners.push((sx, p[ci as usize], p[co as usize]));
        lm.loops.push((format!("lid_upper_{s}"), upper.clone()));
        lm.loops.push((format!("lid_lower_{s}"), lower));
        uppers.push(upper);
    }

    // ---- brows: an arch above each upper lid.
    let mut brow_pts: Vec<Vec3> = Vec::new();
    for (k, &(sx, ci, co)) in corners.iter().enumerate() {
        let s = side(sx);
        let out = sx.signum();
        let width = (co.x - ci.x).abs();
        let top = uppers[k]
            .iter()
            .map(|&v| p[v as usize].y)
            .fold(f32::MIN, f32::max);
        let inner = Vec3::new(ci.x - out * 0.003, top + 0.010, 0.0);
        let peak = Vec3::new(ci.x + out * width * 0.68, top + 0.016, 0.0);
        let outer = Vec3::new(co.x + out * 0.008, co.y + 0.012, 0.0);
        let names = ["inner", "peak", "outer"];
        let mut curve = Vec::new();
        for (j, q) in [inner, peak, outer].iter().enumerate() {
            let v = front_at(q.x, q.y, 0.004);
            lm.points.push((format!("brow_{}_{s}", names[j]), v));
        }
        // The brow curve: a quadratic through inner, peak, outer.
        for i in 0..=10 {
            let t = i as f32 / 10.0;
            let q = if t < 0.6 {
                inner.lerp(peak, t / 0.6)
            } else {
                peak.lerp(outer, (t - 0.6) / 0.4)
            };
            let v = front_at(q.x, q.y, 0.0035);
            if curve.last() != Some(&v) {
                curve.push(v);
                brow_pts.push(p[v as usize]);
            }
        }
        lm.points
            .push((format!("brow_{s}"), curve[curve.len() / 2]));
        lm.loops.push((format!("brow_{s}"), curve));
    }

    // ---- midline profile.
    let mid_front = |ylo: f32, yhi: f32| -> u32 {
        body.iter()
            .copied()
            .filter(|&i| {
                let v = p[i as usize];
                v.x.abs() < 0.004 && v.y >= ylo && v.y <= yhi && nrm[i as usize].z > -0.2
            })
            .max_by(|&i, &j| p[i as usize].z.total_cmp(&p[j as usize].z))
            .unwrap_or(0)
    };
    let nose_tip = mid_front(eye_y - 0.075, eye_y - 0.01);
    let nt = p[nose_tip as usize];
    let bridge = mid_front(eye_y - 0.008, eye_y + 0.006);
    lm.points.push(("nose_tip".into(), nose_tip));
    lm.points.push(("nose_bridge".into(), bridge));
    // Mouth corners from the corner puller.
    let mut mcorner: Vec<Vec3> = Vec::new();
    if let Some(ti) = unit_target(a, "mouth-corner-puller") {
        let m = mags(a, ti);
        for s in [1.0f32, -1.0] {
            if let Some(v) = body
                .iter()
                .copied()
                .filter(|&i| {
                    let q = p[i as usize];
                    q.x * s > 0.012 && q.y < nt.y - 0.01 && q.y > nt.y - 0.07 && q.z > head.z
                })
                .max_by(|&i, &j| m[i as usize].total_cmp(&m[j as usize]))
            {
                // The corner proper: the lip-line vertex nearest it, toward
                // the midline (the puller moves the cheek round it too).
                lm.points.push((format!("mouth_corner_{}", side(s)), v));
                mcorner.push(p[v as usize]);
            }
        }
    }
    let my = if mcorner.is_empty() {
        nt.y - 0.035
    } else {
        mcorner.iter().map(|c| c.y).sum::<f32>() / mcorner.len() as f32
    };
    let lip_top = mid_front(my + 0.002, nt.y - 0.016);
    let lip_bottom = mid_front(my - 0.022, my - 0.002);
    lm.points.push(("lip_top".into(), lip_top));
    lm.points.push(("lip_bottom".into(), lip_bottom));
    let lb = p[lip_bottom as usize];
    let chin = mid_front(lb.y - 0.065, lb.y - 0.025);
    lm.points.push(("chin".into(), chin));

    // ---- ears.
    let mut ear_w = vec![0.0f32; n];
    let mut ear_tops: Vec<Vec3> = Vec::new();
    for k in ["l-ear-wing", "r-ear-wing"] {
        let Some(ti) = key_target(a, k) else { continue };
        let m = mags(a, ti);
        let mx = body.iter().map(|&i| m[i as usize]).fold(0.0, f32::max);
        if mx <= 0.0 {
            continue;
        }
        let mut top: Option<u32> = None;
        for &i in &body {
            let w = smooth(0.45 * mx, 0.65 * mx, m[i as usize]);
            if w > ear_w[i as usize] {
                ear_w[i as usize] = w;
            }
            if w > 0.9 && top.is_none_or(|t| p[i as usize].y > p[t as usize].y) {
                top = Some(i);
            }
        }
        if let Some(t) = top {
            lm.points
                .push((format!("ear_top_{}", side(p[t as usize].x)), t));
            ear_tops.push(p[t as usize]);
        }
    }

    // ---- torso.
    for k in ["nipple-point", "nipple-size"] {
        let Some(ti) = key_target(a, k) else { continue };
        let m = mags(a, ti);
        for s in [1.0f32, -1.0] {
            if let Some(v) = body
                .iter()
                .copied()
                .filter(|&i| p[i as usize].x * s > 0.02)
                .max_by(|&i, &j| m[i as usize].total_cmp(&m[j as usize]))
                && m[v as usize] > 0.0
            {
                lm.points.push((format!("nipple_{}", side(s)), v));
            }
        }
        if lm.point("nipple_l").is_some() {
            break;
        }
    }
    if let Some(ti) = key_target(a, "stomach-navel-in-out") {
        let m = mags(a, ti);
        if let Some(v) = body
            .iter()
            .copied()
            .max_by(|&i, &j| m[i as usize].total_cmp(&m[j as usize]))
        {
            lm.points.push(("navel".into(), v));
        }
    }

    // ---- hairline and scalp.
    let brow_top = brow_pts.iter().map(|b| b.y).fold(eye_y + 0.02, f32::max);
    let ear_top_y = if ear_tops.is_empty() {
        eye_y
    } else {
        ear_tops.iter().map(|e| e.y).sum::<f32>() / ear_tops.len() as f32
    };
    let ear_z = if ear_tops.is_empty() {
        head.z
    } else {
        ear_tops.iter().map(|e| e.z).sum::<f32>() / ear_tops.len() as f32
    };
    let centre = Vec3::new(0.0, ear_top_y, ear_z + 0.01);
    let front_y = brow_top + 0.042;
    let side_y = ear_top_y + 0.012;
    let back_y = ear_top_y - 0.055;
    let hairline_y = |x: f32, z: f32| -> f32 {
        let th = x.atan2(z - centre.z).abs(); // 0 front .. PI back
        let h = std::f32::consts::FRAC_PI_2;
        if th < h * 0.75 {
            // The forehead, then down the temples to the sideburns.
            front_y + (side_y - front_y) * smooth(h * 0.4, h * 0.75, th)
        } else {
            side_y + (back_y - side_y) * smooth(h * 1.2, h * 1.6, th)
        }
    };
    let mut scalp = vec![0.0f32; n];
    for &i in &body {
        let v = p[i as usize];
        if v.y < back_y - 0.03 || (v.x.abs() > 0.13) {
            continue;
        }
        // Stay off the face below the brow line and off the neck front.
        if v.y < brow_top && v.z > ear_z + 0.03 {
            continue;
        }
        let hy = hairline_y(v.x, v.z);
        let w = smooth(hy - 0.004, hy + 0.006, v.y);
        // Not the ears, nor a band round them below their top.
        let w = if ear_w[i as usize] > 0.05 || (v.y < ear_top_y + 0.01 && near_ear(v, &ear_tops)) {
            0.0
        } else {
            w
        };
        scalp[i as usize] = w;
    }
    // The hairline loop: round the head, the vertex on the hairline at each
    // angle.
    let mut hl: Vec<u32> = Vec::new();
    for k in 0..40 {
        let th = (k as f32 / 40.0) * std::f32::consts::TAU;
        let dir = Vec3::new(th.sin(), 0.0, th.cos());
        let best = body
            .iter()
            .copied()
            .filter(|&i| {
                let v = p[i as usize];
                let r = Vec3::new(v.x, 0.0, v.z - centre.z);
                r.length() > 0.03
                    && r.normalize().dot(dir) > 0.985
                    && scalp[i as usize] > 0.2
                    && scalp[i as usize] < 0.9
            })
            .max_by(|&i, &j| {
                let ri = Vec3::new(p[i as usize].x, 0.0, p[i as usize].z - centre.z).length();
                let rj = Vec3::new(p[j as usize].x, 0.0, p[j as usize].z - centre.z).length();
                ri.total_cmp(&rj)
            });
        if let Some(b) = best
            && hl.last() != Some(&b)
        {
            hl.push(b);
        }
    }
    lm.loops.push(("hairline".into(), hl));

    // ---- zones.
    let radial = |c: Vec3, r: f32| -> Vec<f32> {
        let mut w = vec![0.0f32; n];
        for &i in &body {
            let d = p[i as usize].distance(c);
            w[i as usize] = 1.0 - smooth(r * 0.45, r, d);
        }
        w
    };
    let maxz = |a: &mut [f32], b: &[f32]| {
        for (x, y) in a.iter_mut().zip(b) {
            *x = x.max(*y);
        }
    };
    // Lips: an ellipse through the corners and the lip extremes.
    let mut lips = vec![0.0f32; n];
    if mcorner.len() == 2 {
        let cx = (mcorner[0].x + mcorner[1].x) * 0.5;
        let hw = (mcorner[0].x - mcorner[1].x).abs() * 0.5 + 0.001;
        let up = p[lip_top as usize].y - my + 0.0015;
        let dn = my - lb.y + 0.0015;
        let zc = mcorner[0].z.min(mcorner[1].z) - 0.004;
        for &i in &body {
            let v = p[i as usize];
            if v.z < zc || nrm[i as usize].z < -0.3 {
                continue;
            }
            let dy = v.y - my;
            let ry = if dy >= 0.0 { up } else { dn };
            // Pointed corners: the height shrinks toward them.
            let ux = ((v.x - cx) / hw).abs();
            let hy = ry * (1.0 - ux.powf(2.2)).max(0.0).sqrt();
            let e = if hy <= 1e-5 { 2.0 } else { dy.abs() / hy };
            lips[i as usize] = 1.0 - smooth(0.8, 1.15, e.max(ux * 0.9));
        }
    }
    let mut brows = vec![0.0f32; n];
    for &i in &body {
        let v = p[i as usize];
        let d = brow_pts
            .iter()
            .map(|b| b.distance(v))
            .fold(f32::MAX, f32::min);
        brows[i as usize] = 1.0 - smooth(0.003, 0.007, d);
    }
    // Bone-weight zones.
    let bone_sum = |pred: &dyn Fn(&str) -> bool| -> Vec<f32> {
        let mut w = vec![0.0f32; n];
        for (bi, b) in a.bones.iter().enumerate() {
            if pred(&b.name)
                && let Some(ws) = a.weights.get(bi)
            {
                for &(v, x) in ws {
                    w[v as usize] += x;
                }
            }
        }
        w.iter().map(|x| x.clamp(0.0, 1.0)).collect()
    };
    let hands = bone_sum(&|n| {
        n.starts_with("finger")
            || n.starts_with("metacarpal")
            || n.starts_with("wrist")
            || n.starts_with("hand")
    });
    let feet = bone_sum(&|n| n.starts_with("foot") || n.starts_with("toe"));
    // Nails: patches on the back of each finger / toe tip.
    let mut nails = vec![0.0f32; n];
    let mut knuckles = vec![0.0f32; n];
    for s in ["L", "R"] {
        for f in 1..=5 {
            for (tip, base, toe) in [
                (
                    format!("finger{f}-3.{s}"),
                    format!("finger{f}-3.{s}"),
                    false,
                ),
                (
                    if f == 1 {
                        format!("toe1-2.{s}")
                    } else {
                        format!("toe{f}-3.{s}")
                    },
                    String::new(),
                    true,
                ),
            ] {
                let (Some(t), Some(h)) = (
                    joint(&format!("{tip}____tail")),
                    joint(&format!("{tip}____head")),
                ) else {
                    continue;
                };
                let dir = (t - h).normalize_or(Vec3::NEG_Y);
                let dorsal = if toe {
                    Vec3::Y
                } else {
                    let out = Vec3::X * t.x.signum();
                    (out - dir * out.dot(dir)).normalize_or(Vec3::X)
                };
                let len = if toe { 0.011 } else { 0.012 };
                for &i in &body {
                    let v = p[i as usize];
                    let back = (t - v).dot(dir);
                    if v.distance(t) > 0.02 || !(-0.004..len).contains(&back) {
                        continue;
                    }
                    let w = smooth(0.35, 0.7, nrm[i as usize].dot(dorsal))
                        * (1.0 - smooth(len * 0.7, len, back));
                    nails[i as usize] = nails[i as usize].max(w);
                }
                if !toe && !base.is_empty() {
                    let k1 = format!("finger{f}-1.{s}____head");
                    if let Some(kp) = joint(&k1) {
                        let w = radial(kp, 0.014);
                        maxz(&mut knuckles, &w);
                    }
                }
            }
        }
    }
    let mut areola = vec![0.0f32; n];
    for s in ["l", "r"] {
        if let Some(v) = lm.point(&format!("nipple_{s}")) {
            let w = radial(p[v as usize], 0.02);
            maxz(&mut areola, &w);
        }
    }
    let nose = radial(nt + Vec3::new(0.0, 0.006, -0.008), 0.026);
    let mut cheeks = vec![0.0f32; n];
    for s in [1.0f32, -1.0] {
        let c = Vec3::new(s * 0.042, nt.y + 0.006, nt.z - 0.035);
        let mut w = radial(c, 0.03);
        for (i, x) in w.iter_mut().enumerate() {
            if nrm[i].z < 0.1 {
                *x = 0.0;
            }
        }
        maxz(&mut cheeks, &w);
    }
    let mut knees = vec![0.0f32; n];
    let mut elbows = vec![0.0f32; n];
    for s in ["L", "R"] {
        if let Some(k) = joint(&format!("lowerleg01.{s}____head")) {
            let mut w = radial(k + Vec3::Z * 0.04, 0.06);
            for (i, x) in w.iter_mut().enumerate() {
                *x *= smooth(0.0, 0.5, nrm[i].z);
            }
            maxz(&mut knees, &w);
        }
        if let Some(k) = joint(&format!("lowerarm01.{s}____head")) {
            let mut w = radial(k - Vec3::Z * 0.03, 0.05);
            for (i, x) in w.iter_mut().enumerate() {
                *x *= smooth(0.0, 0.5, -nrm[i].z);
            }
            maxz(&mut elbows, &w);
        }
    }
    // Face: in front of the ears, below the hairline, above the jaw.
    let chin_y = p[chin as usize].y;
    let mut face = vec![0.0f32; n];
    let mut tzone = vec![0.0f32; n];
    for &i in &body {
        let v = p[i as usize];
        if v.y < chin_y - 0.015 || v.y > front_y + 0.01 || v.x.abs() > 0.09 {
            continue;
        }
        let w = smooth(ear_z - 0.005, ear_z + 0.02, v.z)
            * (1.0 - scalp[i as usize])
            * (1.0 - ear_w[i as usize]);
        face[i as usize] = w;
        // T-zone: the forehead and the nose ridge.
        let t = if v.y > brow_top {
            1.0 - smooth(0.03, 0.06, v.x.abs())
        } else if v.y > nt.y - 0.01 {
            1.0 - smooth(0.008, 0.018, v.x.abs())
        } else {
            0.0
        };
        tzone[i as usize] = t * w;
    }
    for (nm, w) in [
        ("lips", lips),
        ("face", face),
        ("scalp", scalp),
        ("brows", brows),
        ("nails", nails),
        ("areola", areola),
        ("hands", hands),
        ("feet", feet),
        ("ears", ear_w),
        ("nose", nose),
        ("cheeks", cheeks),
        ("knees", knees),
        ("elbows", elbows),
        ("knuckles", knuckles),
        ("tzone", tzone),
    ] {
        lm.zones.push((nm.into(), w));
    }
    lm
}

trait PipeFront {
    fn pipe_front(self, x: f32, y: f32) -> Option<u32>;
}

impl PipeFront for Vec<(u32, Vec3)> {
    fn pipe_front(self, x: f32, y: f32) -> Option<u32> {
        let zmax = self.iter().map(|c| c.1.z).fold(f32::MIN, f32::max);
        self.iter()
            .filter(|c| c.1.z > zmax - 0.01)
            .min_by(|a, b| {
                let da = (a.1.x - x).powi(2) + (a.1.y - y).powi(2);
                let db = (b.1.x - x).powi(2) + (b.1.y - y).powi(2);
                da.total_cmp(&db)
            })
            .map(|c| c.0)
    }
}

fn near_ear(v: Vec3, tops: &[Vec3]) -> bool {
    tops.iter().any(|t| {
        let dx = (v.x - t.x).abs();
        let dz = (v.z - t.z).abs();
        dx < 0.03 && dz < 0.035 && v.y > t.y - 0.07
    })
}

// ------------------------------------------------------------ zone images

type ImgCache = Vec<((String, usize, u32), Arc<Vec<f32>>)>;
static IMAGES: Mutex<ImgCache> = Mutex::new(Vec::new());

/// A zone rasterized over the template's uv layout as the card domain
/// samples it (`res` x `res`, top row = v 0 in glTF orientation), blurred
/// by `soft` meters (about 2 m of body per uv unit).
pub fn zone_image(zone: &str, res: usize, soft: f32) -> Result<Arc<Vec<f32>>, String> {
    let key = (zone.to_string(), res, soft.to_bits());
    if let Ok(c) = IMAGES.lock()
        && let Some(e) = c.iter().find(|e| e.0 == key)
    {
        return Ok(e.1.clone());
    }
    let a = asset::load()?;
    let lm = get()?;
    let Some(w) = lm.zone(zone) else {
        return Err(format!(
            "zone: `{zone}` is not a zone (one of {})",
            ZONES.join(", ")
        ));
    };
    let mut img = vec![0.0f32; res * res];
    let fr = res as f32;
    for f in a.faces.iter().filter(|f| f.group == 0) {
        for tri in [[0usize, 1, 2], [0, 2, 3]] {
            let uv = tri.map(|k| {
                let u = a.uvs[f.t[k] as usize];
                [u[0] * fr, (1.0 - u[1]) * fr]
            });
            let ww = tri.map(|k| w[f.v[k] as usize]);
            if ww.iter().all(|x| *x <= 0.0) {
                continue;
            }
            raster(&mut img, res, uv, ww);
        }
    }
    let r = (soft.max(0.0) / 2.0 * fr).round() as usize;
    if r > 0 {
        img = blur(&img, res, r);
    }
    let img = Arc::new(img);
    if let Ok(mut c) = IMAGES.lock() {
        c.push((key, img.clone()));
        if c.len() > 24 {
            c.remove(0);
        }
    }
    Ok(img)
}

fn raster(img: &mut [f32], res: usize, p: [[f32; 2]; 3], w: [f32; 3]) {
    let minx = p
        .iter()
        .map(|q| q[0])
        .fold(f32::MAX, f32::min)
        .floor()
        .max(0.0) as usize;
    let maxx = (p.iter().map(|q| q[0]).fold(f32::MIN, f32::max).ceil() as usize).min(res);
    let miny = p
        .iter()
        .map(|q| q[1])
        .fold(f32::MAX, f32::min)
        .floor()
        .max(0.0) as usize;
    let maxy = (p.iter().map(|q| q[1]).fold(f32::MIN, f32::max).ceil() as usize).min(res);
    let area =
        (p[1][0] - p[0][0]) * (p[2][1] - p[0][1]) - (p[2][0] - p[0][0]) * (p[1][1] - p[0][1]);
    if area.abs() < 1e-9 {
        return;
    }
    for y in miny..maxy {
        for x in minx..maxx {
            let (qx, qy) = (x as f32 + 0.5, y as f32 + 0.5);
            let e = |a: [f32; 2], b: [f32; 2]| {
                (b[0] - a[0]) * (qy - a[1]) - (b[1] - a[1]) * (qx - a[0])
            };
            let b0 = e(p[1], p[2]) / area;
            let b1 = e(p[2], p[0]) / area;
            let b2 = e(p[0], p[1]) / area;
            // A texel of slack so seams don't open.
            if b0 < -0.02 || b1 < -0.02 || b2 < -0.02 {
                continue;
            }
            let v = (w[0] * b0 + w[1] * b1 + w[2] * b2).clamp(0.0, 1.0);
            let c = &mut img[y * res + x];
            *c = c.max(v);
        }
    }
}

fn blur(img: &[f32], res: usize, r: usize) -> Vec<f32> {
    let pass = |src: &[f32], horiz: bool| -> Vec<f32> {
        let mut out = vec![0.0f32; src.len()];
        for y in 0..res {
            for x in 0..res {
                let (mut s, mut c) = (0.0, 0.0);
                let at = if horiz { x } else { y };
                for k in at.saturating_sub(r)..=(at + r).min(res - 1) {
                    s += if horiz {
                        src[y * res + k]
                    } else {
                        src[k * res + x]
                    };
                    c += 1.0;
                }
                out[y * res + x] = s / c;
            }
        }
        out
    };
    let h = pass(img, true);
    pass(&h, false)
}

/// Samples a zone image at card uv (meters on a 1 m tile = template uv).
pub fn sample(img: &[f32], res: usize, uv: [f32; 2]) -> f32 {
    let fx = (uv[0].rem_euclid(1.0)) * res as f32 - 0.5;
    let fy = (uv[1].rem_euclid(1.0)) * res as f32 - 0.5;
    let x0 = fx.floor();
    let y0 = fy.floor();
    let (tx, ty) = (fx - x0, fy - y0);
    let at = |x: f32, y: f32| {
        let xi = (x as i64).clamp(0, res as i64 - 1) as usize;
        let yi = (y as i64).clamp(0, res as i64 - 1) as usize;
        img[yi * res + xi]
    };
    let a = at(x0, y0) * (1.0 - tx) + at(x0 + 1.0, y0) * tx;
    let b = at(x0, y0 + 1.0) * (1.0 - tx) + at(x0 + 1.0, y0 + 1.0) * tx;
    a * (1.0 - ty) + b * ty
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn landmarks_found_and_plausible() {
        if !asset::available() {
            return;
        }
        let lm = get().unwrap_or_else(|e| panic!("{e}"));
        let Ok(a) = asset::load() else { return };
        let p = |n: &str| {
            let v = lm.point(n).unwrap_or_else(|| panic!("no landmark {n}"));
            Vec3::from(a.verts[v as usize])
        };
        for n in [
            "brow_inner_l",
            "brow_peak_l",
            "brow_outer_r",
            "eye_corner_in_l",
            "eye_corner_out_r",
            "nose_tip",
            "nose_bridge",
            "mouth_corner_l",
            "mouth_corner_r",
            "lip_top",
            "lip_bottom",
            "chin",
            "ear_top_l",
            "ear_top_r",
            "crown",
            "nipple_l",
            "nipple_r",
            "navel",
        ] {
            let _ = p(n);
        }
        assert!(p("eye_corner_in_l").x > 0.0 && p("eye_corner_in_r").x < 0.0);
        assert!(p("eye_corner_out_l").x > p("eye_corner_in_l").x);
        assert!(p("brow_peak_l").y > p("eye_corner_in_l").y);
        assert!(p("nose_tip").z > p("nose_bridge").z);
        assert!(p("lip_top").y > p("lip_bottom").y);
        assert!(p("chin").y < p("lip_bottom").y);
        assert!(p("crown").y > p("brow_peak_l").y);
        assert!(p("navel").y < p("nipple_l").y);
        for z in ZONES {
            let w = lm.zone(z).unwrap_or_else(|| panic!("no zone {z}"));
            assert!(w.iter().any(|x| *x > 0.5), "zone {z} empty");
        }
        assert!(lm.curve("hairline").is_some_and(|h| h.len() > 12));
        assert!(lm.curve("lid_upper_l").is_some_and(|h| h.len() >= 5));
    }
}
