//! Garment fit: cloth bridges hollows and hangs instead of following
//! every contour of the skin.
//!
//! The eased skin offset (see `shape_sheet`) is split into body segments
//! (trunk, each arm, each leg, by the body's skin weights). Each segment is
//! cut into stations along its axis (world y for the trunk, arc length
//! along the joint chain for limbs); at each station the cross-section's
//! convex hull is taken, widened by the fit's ease allowance (extra
//! circumference / 2 pi on the radius) and, for `hang`, never narrower than
//! the station above it (cloth falls straight down from the widest point:
//! from the bust over the belly, from the seat over the thigh). Every
//! vertex is then pushed out radially toward that profile (never in), the
//! push smoothed over the garment's own edges.
//!
//! Ease allowances (`fit` presets) are the usual pattern-making body
//! allowances (Aldrich, *Metric Pattern Cutting*; Armstrong, *Patternmaking
//! for Fashion Design*): a fitted bodice takes ~5-8 cm at the bust, a
//! classic shirt ~10-15 cm, casual / loose tops 15-25 cm; trousers take
//! 4-6 cm at the thigh fitted (jeans), 8-10 cm regular, 12+ cm loose; a
//! sleeve takes 5-8 cm at the biceps fitted, 10-15 cm regular.

use bevy::math::{Vec2, Vec3};

/// How a garment fits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Fit {
    /// 0 = keep the skin offset, 1 = the full convex profile.
    pub hull: f32,
    /// Extra circumference in meters: trunk (chest / hip), arm (biceps), leg
    /// (thigh).
    pub ease: [f32; 3],
    /// 0..1: how much the trunk falls straight from the widest point above.
    pub hang: f32,
    /// 0..1: how much a trouser leg keeps its thigh width down the leg.
    pub leg_hang: f32,
    /// Leg width kept below the knee (1 = straight to the hem, slacks).
    pub below_knee: f32,
    /// Fold amplitude (meters) at elbows, knees and the waist.
    pub folds: f32,
}

/// Fit words (tight, regular, loose, baggy) for a garment kind: `top`
/// (shirts, dresses), `bottom` (trousers, shorts) or anything else.
pub(super) fn preset(word: &str, kind: &str) -> Result<Option<Fit>, String> {
    let top = kind == "top";
    let f = |hull, chest: f32, arm: f32, thigh: f32, hang, leg_hang, below_knee, folds| Fit {
        hull,
        ease: [chest, arm, thigh],
        hang,
        leg_hang,
        below_knee,
        folds,
    };
    Ok(Some(match word {
        "skin" | "none" => return Ok(None),
        // Knit, close but bridging the deepest hollows (a cleft, the navel).
        "tight" => f(0.5, 0.0, 0.01, 0.0, 0.25, 0.2, 0.0, 0.0),
        "regular" if top => f(1.0, 0.10, 0.08, 0.06, 0.8, 0.0, 0.0, 0.0025),
        "regular" => f(1.0, 0.06, 0.08, 0.08, 0.9, 0.85, 0.75, 0.0025),
        "loose" if top => f(1.0, 0.18, 0.12, 0.10, 1.0, 0.0, 0.0, 0.003),
        "loose" => f(1.0, 0.10, 0.12, 0.10, 1.0, 0.95, 1.0, 0.003),
        "baggy" => f(1.0, 0.28, 0.18, 0.16, 1.0, 1.0, 1.0, 0.004),
        other => {
            return Err(format!(
                "fit: unknown `{other}` (tight, regular, loose, baggy or skin)"
            ));
        }
    }))
}

/// Segment of a vertex: 0 trunk, 1 / 2 left / right arm, 3 / 4 left / right
/// leg; the value is that segment's share of the vertex's weights.
pub(super) type Membership = [f32; 5];

/// Joint chains: arms [shoulder, elbow, wrist], legs [thigh, knee, ankle],
/// left then right; plus the shoulder height (trunk push fades above it).
pub(super) struct Axes {
    pub arms: Option<[[Vec3; 3]; 2]>,
    pub legs: Option<[[Vec3; 3]; 2]>,
    pub shoulder_y: f32,
}

const ANG: usize = 48;
const BIN: f32 = 0.02;

/// Where a vertex sits in a segment: station coordinate, 2D point in the
/// cross-section plane, the 3D radial unit direction and how much it may
/// move (its normal facing out across the axis).
struct Local {
    s: f32,
    q: Vec2,
    dir: Vec3,
    gate: f32,
}

fn chain_local(p: Vec3, n: Vec3, c: &[Vec3; 3]) -> Local {
    let l1 = (c[1] - c[0]).length().max(1e-4);
    let l2 = (c[2] - c[1]).length().max(1e-4);
    let seg = |a: Vec3, b: Vec3| {
        let ab = b - a;
        let t = ((p - a).dot(ab) / ab.length_squared().max(1e-9)).clamp(0.0, 1.0);
        (t, a + ab * t, ab.normalize_or(Vec3::Y))
    };
    let (t1, q1, d1) = seg(c[0], c[1]);
    let (t2, q2, d2) = seg(c[1], c[2]);
    let (s, q, d) = if (p - q1).length_squared() <= (p - q2).length_squared() {
        (t1 * l1, q1, d1)
    } else {
        (l1 + t2 * l2, q2, d2)
    };
    let r = p - q;
    let radial = (r - d * r.dot(d)).normalize_or(Vec3::X);
    let e1 = (Vec3::Z - d * d.z).normalize_or(Vec3::X);
    let e2 = d.cross(e1);
    Local {
        s,
        q: Vec2::new(r.dot(e1), r.dot(e2)),
        dir: radial,
        gate: n.dot(radial).clamp(0.0, 1.0),
    }
}

/// The segment's axis at a vertex (world y for the trunk).
fn axis_of(chain: &Option<[Vec3; 3]>, l: &Local) -> Vec3 {
    match chain {
        None => Vec3::Y,
        Some(c) => {
            let l1 = (c[1] - c[0]).length();
            if l.s <= l1 {
                (c[1] - c[0]).normalize_or(Vec3::Y)
            } else {
                (c[2] - c[1]).normalize_or(Vec3::Y)
            }
        }
    }
}

/// Radius of the convex polygon `h` (around `c`) toward unit `d`.
fn ray(h: &[Vec2], c: Vec2, d: Vec2) -> f32 {
    let mut best = 0.0f32;
    for k in 0..h.len() {
        let (p, q) = (h[k] - c, h[(k + 1) % h.len()] - c);
        let e = q - p;
        let den = d.perp_dot(e);
        if den.abs() < 1e-12 {
            continue;
        }
        let t = p.perp_dot(e) / den;
        let u = p.perp_dot(d) / den;
        if t > 0.0 && (-1e-4..=1.0001).contains(&u) {
            best = best.max(t);
        }
    }
    best
}

fn unit(j: usize) -> Vec2 {
    let a = j as f32 / ANG as f32 * std::f32::consts::TAU;
    Vec2::new(a.cos(), a.sin())
}

/// Pushes `p` (eased offset positions, with body normals `nrm`) out to the
/// fitted profile. `mem` gives each vertex's segments.
pub(super) fn apply(
    p: &mut [Vec3],
    nrm: &[Vec3],
    tris: &[[u32; 3]],
    mem: &[Membership],
    axes: &Axes,
    fit: &Fit,
) {
    let n = p.len();
    let mut disp = vec![Vec3::ZERO; n];
    for seg in 0..5 {
        let chain = match seg {
            0 => None,
            1 | 2 => match &axes.arms {
                Some(a) => Some(a[seg - 1]),
                None => continue,
            },
            _ => match &axes.legs {
                Some(l) => Some(l[seg - 3]),
                None => continue,
            },
        };
        let idx: Vec<usize> = (0..n).filter(|i| mem[*i][seg] > 0.05).collect();
        if idx.len() < 12 {
            continue;
        }
        let loc: Vec<Local> = idx
            .iter()
            .map(|&i| match &chain {
                None => Local {
                    s: -p[i].y,
                    q: Vec2::new(p[i].x, p[i].z),
                    dir: Vec3::ZERO,
                    gate: 1.0,
                },
                Some(c) => chain_local(p[i], nrm[i], c),
            })
            .collect();
        let (smin, smax) = loc
            .iter()
            .fold((f32::MAX, f32::MIN), |(a, b), l| (a.min(l.s), b.max(l.s)));
        let nst = (((smax - smin) / BIN).ceil() as usize + 1).max(1);
        let st = |s: f32| ((s - smin) / BIN).clamp(0.0, (nst - 1) as f32);
        // Hull points per station (members only).
        let mut pts: Vec<Vec<Vec2>> = vec![Vec::new(); nst];
        for (k, &i) in idx.iter().enumerate() {
            if mem[i][seg] > 0.5 {
                pts[st(loc[k].s).round() as usize].push(loc[k].q);
            }
        }
        let mut prof: Vec<Option<(Vec2, Vec<f32>)>> = (0..nst)
            .map(|k| {
                let mut all = Vec::new();
                for dk in [-1i32, 0, 1] {
                    let kk = k as i32 + dk;
                    if (0..nst as i32).contains(&kk) {
                        all.extend_from_slice(&pts[kk as usize]);
                    }
                }
                if all.len() < 6 {
                    return None;
                }
                let h = super::hull2(all);
                if h.len() < 3 {
                    return None;
                }
                let c = match &chain {
                    // The limb axis is the centre: hulls skewed by a fold
                    // shouldn't move it.
                    Some(_) => Vec2::ZERO,
                    None => h.iter().copied().sum::<Vec2>() / h.len() as f32,
                };
                Some((c, (0..ANG).map(|j| ray(&h, c, unit(j))).collect()))
            })
            .collect();
        // Fill gaps from the nearest station.
        for k in 0..nst {
            if prof[k].is_some() {
                continue;
            }
            let near = (1..nst).find_map(|d| {
                [k.checked_sub(d), Some(k + d)]
                    .into_iter()
                    .flatten()
                    .find(|kk| *kk < nst && prof[*kk].is_some())
            });
            if let Some(kk) = near {
                prof[k] = prof[kk].clone();
            }
        }
        let mut prof: Vec<(Vec2, Vec<f32>)> = prof.into_iter().flatten().collect();
        if prof.len() != nst {
            continue;
        }
        // Smooth around the ring and along the stations.
        let smooth = |prof: &mut Vec<(Vec2, Vec<f32>)>| {
            for (_, r) in prof.iter_mut() {
                for _ in 0..2 {
                    let pr = r.clone();
                    for j in 0..ANG {
                        r[j] = (pr[(j + ANG - 1) % ANG] + 2.0 * pr[j] + pr[(j + 1) % ANG]) / 4.0;
                    }
                }
            }
            for _ in 0..2 {
                let pr = prof.clone();
                for k in 0..nst {
                    let (a, b) = (k.saturating_sub(1), (k + 1).min(nst - 1));
                    prof[k].0 = (pr[a].0 + pr[k].0 * 2.0 + pr[b].0) / 4.0;
                    for j in 0..ANG {
                        prof[k].1[j] = (pr[a].1[j] + 2.0 * pr[k].1[j] + pr[b].1[j]) / 4.0;
                    }
                }
            }
        };
        smooth(&mut prof);
        // Hang: stations run top to bottom (trunk s = -y; limbs root to tip).
        let (hang, start, ease) = match seg {
            0 => (fit.hang, 0.0, fit.ease[0]),
            1 | 2 => (0.0, 0.0, fit.ease[1]),
            _ => (fit.leg_hang, 0.12, fit.ease[2]),
        };
        if hang > 0.0 {
            let knee = chain.map(|c| (c[1] - c[0]).length());
            for k in 1..nst {
                let s = smin + k as f32 * BIN;
                if s - smin < start {
                    continue;
                }
                let h = match knee {
                    Some(kn) if s > kn => hang * fit.below_knee,
                    _ => hang,
                };
                let (ca, ra) = prof[k - 1].clone();
                let c = prof[k].0;
                for j in 0..ANG {
                    let u = unit(j);
                    let from_above = (ca + u * ra[j] - c).dot(u);
                    let r = &mut prof[k].1[j];
                    if from_above > *r {
                        *r += (from_above - *r) * h;
                    }
                }
            }
            smooth(&mut prof);
        }
        let grow = ease / std::f32::consts::TAU;
        for (k, &i) in idx.iter().enumerate() {
            let l = &loc[k];
            let f = st(l.s);
            let (k0, k1) = (f.floor() as usize, (f.ceil() as usize).min(nst - 1));
            let t = f - k0 as f32;
            let c = prof[k0].0.lerp(prof[k1].0, t);
            let v = l.q - c;
            let r = v.length();
            if r < 1e-5 {
                continue;
            }
            let u = v / r;
            let a = u.y.atan2(u.x).rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU
                * ANG as f32;
            let (j0, ja) = (a.floor() as usize % ANG, a.fract());
            let j1 = (j0 + 1) % ANG;
            let rr = |kk: usize| prof[kk].1[j0] * (1.0 - ja) + prof[kk].1[j1] * ja;
            let target = rr(k0) * (1.0 - t) + rr(k1) * t;
            let hull_push = (target - r).max(0.0) * fit.hull;
            let mut push = hull_push;
            // Folds: soft rings bunching at the elbow / knee.
            if let Some(cc) = &chain
                && fit.folds > 0.0
            {
                let joint_s = (cc[1] - cc[0]).length();
                let band = (1.0 - (l.s - joint_s).abs() / 0.09).max(0.0);
                let wave = (l.s / 0.028 * std::f32::consts::TAU + a * 0.2).sin();
                push += fit.folds * band * band * wave.max(0.0);
            }
            let max = if seg == 0 { 0.07 } else { 0.045 };
            let mut gate = l.gate.clamp(0.0, 1.0);
            // Over the shoulders the shirt lies on the body.
            let lie = if seg == 0 {
                ((axes.shoulder_y - 0.02 - p[i].y) / 0.06).clamp(0.0, 1.0)
            } else {
                1.0
            };
            if seg == 0 {
                gate = Vec2::new(nrm[i].x, nrm[i].z).dot(u).max(0.0);
            }
            let gate = gate.powf(0.5) * lie;
            // The ease allowance goes all round (along the body normal's
            // side), the hull push only where the cloth faces out.
            let push =
                push.min(max) * gate + grow * lie * (1.0 - nrm[i].dot(axis_of(&chain, l)).abs());
            let dir = match &chain {
                None => Vec3::new(u.x, 0.0, u.y),
                Some(_) => l.dir,
            };
            disp[i] += dir * push * mem[i][seg];
        }
    }
    // Smooth the push over the garment's edges.
    let (nb, _) = super::neighbours(n, tris);
    for _ in 0..8 {
        let prev = disp.clone();
        for i in 0..n {
            if nb[i].is_empty() {
                continue;
            }
            let m = nb[i].iter().map(|j| prev[*j as usize]).sum::<Vec3>() / nb[i].len() as f32;
            disp[i] = prev[i].lerp(m, 0.5);
        }
    }
    for i in 0..n {
        p[i] += disp[i];
    }
}

/// Cloth tension: dips bridge (a vertex below its neighbours' average rises
/// to it; never the other way, so convex shapes keep their size), then a
/// light plain smoothing evens small bumps; never closer than `offset`
/// over the source point (`base` along the body normal `nrm`).
pub(super) fn drape(
    p: &mut [Vec3],
    base: &[Vec3],
    nrm: &[Vec3],
    tris: &[[u32; 3]],
    offset: f32,
    fit: &Fit,
) {
    let n = p.len();
    let (nb, border) = super::neighbours(n, tris);
    let floor = |p: &mut [Vec3]| {
        for i in 0..n {
            let d = (p[i] - base[i]).dot(nrm[i]);
            if d < offset {
                p[i] += nrm[i] * (offset - d);
            }
        }
    };
    let bridge = (40.0 * fit.hull).round() as usize;
    for it in 0..bridge + 6 {
        let gn = super::vertex_normals(p, tris);
        let prev = p.to_vec();
        for i in 0..n {
            let ns: Vec<u32> = if border[i] {
                nb[i]
                    .iter()
                    .copied()
                    .filter(|j| border[*j as usize])
                    .collect()
            } else {
                nb[i].clone()
            };
            if ns.is_empty() {
                continue;
            }
            let m = ns.iter().map(|j| prev[*j as usize]).sum::<Vec3>() / ns.len() as f32;
            let d = m - prev[i];
            if it < bridge {
                let out = d.dot(gn[i]);
                if out > 0.0 {
                    p[i] = prev[i] + d * 0.6;
                }
            } else {
                p[i] = prev[i] + d * 0.35;
            }
        }
        floor(p);
    }
    // Even the spacing along the surface: bridging gathers vertices in the
    // hollows it spans (the armpit), and short edges there stretch many
    // times over once the arm moves.
    for _ in 0..12 {
        let gn = super::vertex_normals(p, tris);
        let prev = p.to_vec();
        for i in 0..n {
            if border[i] || nb[i].is_empty() {
                continue;
            }
            let m = nb[i].iter().map(|j| prev[*j as usize]).sum::<Vec3>() / nb[i].len() as f32;
            let d = m - prev[i];
            p[i] = prev[i] + (d - gn[i] * d.dot(gn[i])) * 0.5;
        }
        floor(p);
    }
}

/// Bridging only where `wgt` > 0 (scaled by it), `iters` rounds.
pub(super) fn bridge(
    p: &mut [Vec3],
    base: &[Vec3],
    nrm: &[Vec3],
    tris: &[[u32; 3]],
    offset: f32,
    wgt: &[f32],
    iters: usize,
) {
    let n = p.len();
    let (nb, border) = super::neighbours(n, tris);
    let act: Vec<usize> = (0..n).filter(|i| wgt[*i] > 0.0 && !border[*i]).collect();
    if act.is_empty() {
        return;
    }
    for _ in 0..iters {
        let gn = super::vertex_normals(p, tris);
        let prev = p.to_vec();
        for &i in &act {
            let m = nb[i].iter().map(|j| prev[*j as usize]).sum::<Vec3>() / nb[i].len() as f32;
            let d = m - prev[i];
            if d.dot(gn[i]) > 0.0 {
                p[i] = prev[i] + d * 0.6 * wgt[i];
            }
        }
        for &i in &act {
            let d = (p[i] - base[i]).dot(nrm[i]);
            if d < offset {
                p[i] += nrm[i] * (offset - d);
            }
        }
    }
}
