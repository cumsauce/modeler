//! Curves and skinning: smooth paths through control points (centripetal
//! Catmull-Rom), rotation-minimising frames along a path, `loft` (a closed
//! solid skinned through cross-sections) and the placements behind
//! `path_array`.

use bevy::math::{Mat3, Quat, Vec2, Vec3};
use serde::{Deserialize, Serialize};

use crate::{ModelerError, Part};

/// One cross-section of a loft: a closed ring placed at `at` (straight
/// lofts: `[a, b]` maps to world x / z before `rotation`) or at fraction
/// `t` along the loft's path (`[a, b]` along the path frame's normal /
/// binormal, like `sweep`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LoftSection {
    pub profile: Vec<[f32; 2]>,
    #[serde(default)]
    pub at: [f32; 3],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotation: Option<[f32; 4]>,
    #[serde(default = "one2", skip_serializing_if = "is_one2")]
    pub scale: [f32; 2],
}

fn one2() -> [f32; 2] {
    [1.0, 1.0]
}

fn is_one2(s: &[f32; 2]) -> bool {
    *s == [1.0, 1.0]
}

pub fn one() -> f32 {
    1.0
}

pub fn is_one(x: &f32) -> bool {
    *x == 1.0
}

pub fn yes() -> bool {
    true
}

pub fn is_yes(b: &bool) -> bool {
    *b
}

/// A path through `ctrl`: the points themselves, or with `smooth` a
/// centripetal Catmull-Rom curve through every control point, `samples`
/// segments per span (0 = by the bend, about one per 5 degrees). Closed
/// paths wrap and do not repeat the first point.
pub fn sample_path(ctrl: &[[f32; 3]], closed: bool, smooth: bool, samples: u32) -> Vec<[f32; 3]> {
    if !smooth || ctrl.len() < 2 || (closed && ctrl.len() < 3) {
        return ctrl.to_vec();
    }
    let p: Vec<Vec3> = ctrl.iter().map(|c| Vec3::from(*c)).collect();
    let n = p.len();
    let at = |i: isize| -> Vec3 {
        if closed {
            p[i.rem_euclid(n as isize) as usize]
        } else if i < 0 {
            2.0 * p[0] - p[1]
        } else if i as usize >= n {
            2.0 * p[n - 1] - p[n - 2]
        } else {
            p[i as usize]
        }
    };
    let spans = if closed { n } else { n - 1 };
    let mut out = Vec::new();
    for s in 0..spans {
        let i = s as isize;
        let (p0, p1, p2, p3) = (at(i - 1), at(i), at(i + 1), at(i + 2));
        let k = if samples > 0 {
            samples
        } else {
            let t1 = (p2 - p0).normalize_or_zero();
            let t2 = (p3 - p1).normalize_or_zero();
            let deg = t1.dot(t2).clamp(-1.0, 1.0).acos().to_degrees();
            ((deg / 5.0).ceil() as u32).clamp(1, 64)
        };
        for j in 0..k {
            out.push(centripetal(p0, p1, p2, p3, j as f32 / k as f32).to_array());
        }
    }
    if !closed {
        out.push(ctrl[n - 1]);
    }
    out
}

/// Centripetal Catmull-Rom between p1 (u = 0) and p2 (u = 1).
fn centripetal(p0: Vec3, p1: Vec3, p2: Vec3, p3: Vec3, u: f32) -> Vec3 {
    let knot = |a: Vec3, b: Vec3| (a - b).length().sqrt().max(1e-6);
    let t0 = 0.0;
    let t1 = t0 + knot(p0, p1);
    let t2 = t1 + knot(p1, p2);
    let t3 = t2 + knot(p2, p3);
    barry_goldman([p0, p1, p2, p3], [t0, t1, t2, t3], t1 + (t2 - t1) * u)
}

/// Catmull-Rom with explicit knots (Barry-Goldman pyramid), evaluated at
/// `t` in [k1, k2]. Works on any vector type through `lerp`.
fn barry_goldman<T>(p: [T; 4], k: [f32; 4], t: f32) -> T
where
    T: Copy + std::ops::Mul<f32, Output = T> + std::ops::Add<Output = T>,
{
    let mix = |a: T, b: T, ka: f32, kb: f32| {
        let d = kb - ka;
        if d.abs() < 1e-9 {
            a
        } else {
            a * ((kb - t) / d) + b * ((t - ka) / d)
        }
    };
    let a1 = mix(p[0], p[1], k[0], k[1]);
    let a2 = mix(p[1], p[2], k[1], k[2]);
    let a3 = mix(p[2], p[3], k[2], k[3]);
    let b1 = mix(a1, a2, k[0], k[2]);
    let b2 = mix(a2, a3, k[1], k[3]);
    mix(b1, b2, k[1], k[2])
}

/// Rotation-minimising frames `(tangent, normal, binormal)` at each path
/// point (normal starts as world up, or +z on a vertical start).
pub fn frames(pts: &[Vec3], closed: bool) -> Vec<(Vec3, Vec3, Vec3)> {
    let n = pts.len();
    let tangent = |i: usize| -> Vec3 {
        let prev = if i > 0 {
            Some(pts[i] - pts[i - 1])
        } else if closed {
            Some(pts[0] - pts[n - 1])
        } else {
            None
        };
        let next = if i + 1 < n {
            Some(pts[i + 1] - pts[i])
        } else if closed {
            Some(pts[0] - pts[n - 1])
        } else {
            None
        };
        match (prev, next) {
            (Some(a), Some(b)) => (a.normalize_or_zero() + b.normalize_or_zero())
                .normalize_or(b.normalize_or(Vec3::Z)),
            (Some(a), None) => a.normalize_or(Vec3::Z),
            (None, Some(b)) => b.normalize_or(Vec3::Z),
            (None, None) => Vec3::Z,
        }
    };
    if n == 0 {
        return Vec::new();
    }
    let t0 = tangent(0);
    let seed = if t0.y.abs() < 0.9 { Vec3::Y } else { Vec3::Z };
    let mut normal = (seed - t0 * seed.dot(t0)).normalize();
    let mut prev_t = t0;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let t = tangent(i);
        let rot = Quat::from_rotation_arc(prev_t, t);
        normal = (rot * normal - t * (rot * normal).dot(t)).normalize_or(normal);
        out.push((t, normal, t.cross(normal)));
        prev_t = t;
    }
    out
}

/// Cumulative arc length at each point (a closed path adds the closing
/// segment as one more entry).
fn arc_lengths(pts: &[Vec3], closed: bool) -> Vec<f32> {
    let mut d = vec![0.0];
    let mut acc = 0.0;
    let m = if closed { pts.len() } else { pts.len() - 1 };
    for i in 0..m {
        acc += (pts[(i + 1) % pts.len()] - pts[i]).length();
        d.push(acc);
    }
    d
}

/// The point and frame at arc length `s` along a sampled path.
fn frame_at(
    pts: &[Vec3],
    fr: &[(Vec3, Vec3, Vec3)],
    lens: &[f32],
    closed: bool,
    s: f32,
) -> (Vec3, Vec3, Vec3, Vec3) {
    let n = pts.len();
    let segs = lens.len() - 1;
    let mut i = lens.partition_point(|&l| l <= s).saturating_sub(1);
    i = i.min(segs.saturating_sub(1));
    let (a, b) = (
        i,
        if closed {
            (i + 1) % n
        } else {
            (i + 1).min(n - 1)
        },
    );
    let len = lens[i + 1] - lens[i];
    let f = if len > 1e-9 {
        ((s - lens[i]) / len).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let p = pts[a].lerp(pts[b], f);
    let t = fr[a].0.lerp(fr[b].0, f).normalize_or(fr[a].0);
    let nrm = fr[a].1.lerp(fr[b].1, f);
    let nrm = (nrm - t * nrm.dot(t)).normalize_or(fr[a].1);
    (p, t, nrm, t.cross(nrm))
}

/// Where `path_array` puts copy i: a world point and the rotation taking
/// the source's +x to the path tangent and +y to the frame normal (world
/// up on a level path), each copy rolled `roll_step` more degrees about the
/// tangent. With `count` 0, `spacing` picks it (evenly spaced, both ends
/// of an open path get a copy).
pub fn placements(
    path: &[[f32; 3]],
    closed: bool,
    count: u32,
    spacing: f32,
    roll_step: f32,
) -> Result<Vec<(Vec3, Quat)>, String> {
    let pts: Vec<Vec3> = path.iter().map(|p| Vec3::from(*p)).collect();
    if pts.len() < 2 {
        return Err("path needs at least 2 points".into());
    }
    if pts.windows(2).any(|w| (w[1] - w[0]).length() < 1e-9) {
        return Err("path has repeated points".into());
    }
    let lens = arc_lengths(&pts, closed);
    let total = lens[lens.len() - 1];
    let n = count_for(total, closed, count, spacing)?;
    let fr = frames(&pts, closed);
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..n {
        let s = if closed {
            total * i as f32 / n as f32
        } else if n == 1 {
            0.0
        } else {
            total * i as f32 / (n - 1) as f32
        };
        let (p, t, nrm, bin) = frame_at(&pts, &fr, &lens, closed, s);
        let r = Quat::from_mat3(&Mat3::from_cols(t, nrm, bin)).normalize()
            * Quat::from_rotation_x((roll_step * i as f32).to_radians());
        out.push((p, r));
    }
    Ok(out)
}

/// How many copies a path_array makes (also used for its output names).
pub fn count_for(total: f32, closed: bool, count: u32, spacing: f32) -> Result<u32, String> {
    if count > 0 {
        return Ok(count);
    }
    if spacing.is_nan() || spacing <= 0.0 {
        return Err("give count (>= 1) or spacing (> 0)".into());
    }
    let gaps = (total / spacing).round().max(1.0) as u32;
    let n = if closed { gaps } else { gaps + 1 };
    if n > 10_000 {
        return Err(format!(
            "spacing {spacing} along {total:.2} m makes {n} copies (max 10000)"
        ));
    }
    Ok(n)
}

/// Length of a (sampled) path, for output naming.
pub fn path_length(path: &[[f32; 3]], closed: bool) -> f32 {
    let pts: Vec<Vec3> = path.iter().map(|p| Vec3::from(*p)).collect();
    if pts.len() < 2 {
        return 0.0;
    }
    arc_lengths(&pts, closed).last().copied().unwrap_or(0.0)
}

fn signed_area(r: &[Vec2]) -> f32 {
    (0..r.len())
        .map(|i| r[i].perp_dot(r[(i + 1) % r.len()]))
        .sum::<f32>()
        * 0.5
}

/// `m` points around a ring. With no more vertices than `m`, every vertex
/// is kept (corners stay sharp) and the extra points go to edges by length;
/// otherwise the ring is resampled evenly by arc length.
fn resample(ring: &[Vec2], m: usize) -> Vec<Vec2> {
    let n = ring.len();
    let edge = |i: usize| (ring[(i + 1) % n] - ring[i]).length();
    let perim: f32 = (0..n).map(edge).sum();
    if n <= m {
        let extra = m - n;
        let share: Vec<f32> = (0..n).map(|i| edge(i) / perim * extra as f32).collect();
        let mut k: Vec<usize> = share.iter().map(|s| s.floor() as usize).collect();
        let mut left = extra - k.iter().sum::<usize>();
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by(|&a, &b| {
            (share[b] - share[b].floor()).total_cmp(&(share[a] - share[a].floor()))
        });
        for &i in &order {
            if left == 0 {
                break;
            }
            k[i] += 1;
            left -= 1;
        }
        let mut out = Vec::with_capacity(m);
        for i in 0..n {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            for j in 0..=k[i] {
                out.push(a.lerp(b, j as f32 / (k[i] + 1) as f32));
            }
        }
        out
    } else {
        let mut out = Vec::with_capacity(m);
        let mut e = 0;
        let mut acc = 0.0;
        for j in 0..m {
            let s = perim * j as f32 / m as f32;
            while e + 1 < n && acc + edge(e) < s {
                acc += edge(e);
                e += 1;
            }
            let l = edge(e).max(1e-12);
            out.push(ring[e].lerp(ring[(e + 1) % n], ((s - acc) / l).clamp(0.0, 1.0)));
        }
        out
    }
}

/// The cyclic shift of `b` that best matches `a` point for point (both
/// centred and normalised by size), so lofts don't twist.
fn best_shift<T: Copy>(a: &[T], b: &[T], sub: impl Fn(T, T) -> f32) -> usize {
    let m = a.len();
    (0..m)
        .min_by(|&s1, &s2| {
            let cost = |s: usize| (0..m).map(|j| sub(a[j], b[(j + s) % m])).sum::<f32>();
            cost(s1).total_cmp(&cost(s2))
        })
        .unwrap_or(0)
}

fn reverse_keep_first<T: Copy>(r: &mut [T]) {
    if r.len() > 1 {
        r[1..].reverse();
    }
}

fn normalise2(r: &[Vec2]) -> Vec<Vec2> {
    let c = r.iter().copied().sum::<Vec2>() / r.len() as f32;
    let s = (r.iter().map(|p| (*p - c).length_squared()).sum::<f32>() / r.len() as f32)
        .sqrt()
        .max(1e-9);
    r.iter().map(|p| (*p - c) / s).collect()
}

fn normalise3(r: &[Vec3]) -> Vec<Vec3> {
    let c = r.iter().copied().sum::<Vec3>() / r.len() as f32;
    let s = (r.iter().map(|p| (*p - c).length_squared()).sum::<f32>() / r.len() as f32)
        .sqrt()
        .max(1e-9);
    r.iter().map(|p| (*p - c) / s).collect()
}

/// Options of a loft beyond its sections.
pub struct LoftOpts<'a> {
    pub path: &'a [[f32; 3]],
    pub smooth: bool,
    pub samples: u32,
    pub steps: u32,
    pub round: f32,
}

/// A closed solid skinned through `sections` (see [`LoftSection`]).
pub fn loft(name: &str, sections: &[LoftSection], o: &LoftOpts) -> Result<Part, ModelerError> {
    let err = |m: String| ModelerError::Csg(format!("loft `{name}`: {m}"));
    if sections.len() < 2 {
        return Err(err(format!(
            "needs at least 2 sections, got {}",
            sections.len()
        )));
    }
    // Section rings in 2D, CCW, scaled.
    let mut rings2: Vec<Vec<Vec2>> = Vec::with_capacity(sections.len());
    for (k, s) in sections.iter().enumerate() {
        if s.profile.len() < 3 || s.profile.iter().flatten().any(|c| !c.is_finite()) {
            return Err(err(format!(
                "section {k}: the profile needs at least 3 finite points"
            )));
        }
        let rr = crate::csg::rounded_rings(std::slice::from_ref(&s.profile), o.round);
        let [ring] = rr.as_slice() else {
            return Err(err(format!(
                "section {k}: the profile must be one simple ring (round may have split it)"
            )));
        };
        let mut r: Vec<Vec2> = ring
            .iter()
            .map(|p| Vec2::new(p[0] as f32, p[1] as f32))
            .collect();
        if signed_area(&r).abs() < 1e-12 {
            return Err(err(format!("section {k}: the profile has no area")));
        }
        if signed_area(&r) < 0.0 {
            r.reverse();
        }
        if s.scale.iter().any(|v| !v.is_finite() || *v < 0.0) {
            return Err(err(format!("section {k}: scale must be >= 0")));
        }
        rings2.push(r);
    }
    let max_n = rings2.iter().map(Vec::len).max().unwrap_or(3);
    let m = if o.samples > 0 {
        o.samples as usize
    } else {
        max_n.max(32)
    };
    if !(3..=4096).contains(&m) {
        return Err(err(format!("samples must be 3..4096, got {m}")));
    }
    let mut rings2: Vec<Vec<Vec2>> = rings2
        .iter()
        .zip(sections)
        .map(|(r, s)| {
            resample(r, m)
                .into_iter()
                .map(|p| p * Vec2::from(s.scale))
                .collect()
        })
        .collect();
    let steps = if o.steps > 0 {
        o.steps
    } else if o.smooth {
        12
    } else {
        1
    };
    // Each section a point too small to have area becomes an apex.
    let apex: Vec<bool> = rings2.iter().map(|r| signed_area(r) < 1e-12).collect();
    if apex[1..apex.len() - 1].iter().any(|a| *a) {
        return Err(err(
            "only the first or last section may shrink to a point (scale 0)".into(),
        ));
    }
    let (rings3, dirs) = if o.path.is_empty() {
        straight(sections, &mut rings2, &apex, o.smooth, steps).map_err(err)?
    } else {
        along_path(sections, &mut rings2, &apex, o, steps).map_err(err)?
    };
    let first = (!apex[0]).then(|| rings2[0].clone());
    let last = (!apex[apex.len() - 1]).then(|| rings2[rings2.len() - 1].clone());
    skin(name, &rings3, first.as_deref(), last.as_deref(), dirs).map_err(err)
}

type Rings = (Vec<Vec<Vec3>>, (Vec3, Vec3));

/// Sections placed in the world (`at`, `rotation`); rings interpolated per
/// point between them.
fn straight(
    sections: &[LoftSection],
    rings2: &mut [Vec<Vec2>],
    apex: &[bool],
    smooth: bool,
    steps: u32,
) -> Result<Rings, String> {
    let k = sections.len();
    let centre: Vec<Vec3> = sections.iter().map(|s| Vec3::from(s.at)).collect();
    for i in 1..k {
        if (centre[i] - centre[i - 1]).length() < 1e-6 {
            return Err(format!(
                "sections {} and {i} are at the same place: give each its own `at` / `y`",
                i - 1
            ));
        }
    }
    let rot: Vec<Quat> = sections
        .iter()
        .map(|s| {
            s.rotation
                .map_or(Quat::IDENTITY, crate::quat_of)
                .normalize()
        })
        .collect();
    let world = |i: usize, r: &[Vec2]| -> Vec<Vec3> {
        r.iter()
            .map(|p| centre[i] + rot[i] * Vec3::new(p.x, 0.0, p.y))
            .collect()
    };
    let mut w: Vec<Vec<Vec3>> = Vec::with_capacity(k);
    for i in 0..k {
        // Wind each ring counter-clockwise seen from the direction of travel.
        let fwd = centre[(i + 1).min(k - 1)] - centre[i.saturating_sub(1)];
        let normal = rot[i] * Vec3::NEG_Y; // x cross z: CCW (a, b) faces it
        if normal.dot(fwd) < 0.0 {
            reverse_keep_first(&mut rings2[i]);
        }
        if i > 0 && !apex[i] && !apex[i - 1] {
            let a = normalise3(&w[i - 1]);
            let b = normalise3(&world(i, &rings2[i]));
            let s = best_shift(&a, &b, |x, y| (x - y).length_squared());
            rings2[i].rotate_left(s);
        }
        w.push(world(i, &rings2[i]));
    }
    // Knots: centripetal over the section centres.
    let mut knots = vec![0.0f32];
    for i in 1..k {
        let d = (centre[i] - centre[i - 1]).length().sqrt();
        knots.push(knots[i - 1] + d);
    }
    let m = rings2[0].len();
    let mut out = Vec::new();
    for span in 0..k - 1 {
        for st in 0..steps {
            let f = st as f32 / steps as f32;
            out.push(interp(&w, &knots, span, f, smooth, m));
        }
    }
    out.push(w[k - 1].clone());
    let d0 = centre[1] - centre[0];
    let d1 = centre[k - 1] - centre[k - 2];
    Ok((out, (d0, d1)))
}

/// Point-wise interpolation of rings between section `span` and `span + 1`.
fn interp<T>(w: &[Vec<T>], knots: &[f32], span: usize, f: f32, smooth: bool, m: usize) -> Vec<T>
where
    T: Copy
        + std::ops::Mul<f32, Output = T>
        + std::ops::Add<Output = T>
        + std::ops::Sub<Output = T>,
{
    let k = w.len();
    let t = knots[span] + (knots[span + 1] - knots[span]) * f;
    (0..m)
        .map(|j| {
            let p1 = w[span][j];
            let p2 = w[span + 1][j];
            if !smooth || f == 0.0 {
                return if f == 0.0 {
                    p1
                } else {
                    p1 * (1.0 - f) + p2 * f
                };
            }
            let (p0, k0) = if span > 0 {
                (w[span - 1][j], knots[span - 1])
            } else {
                (p1 + (p1 - p2), knots[0] - (knots[1] - knots[0]))
            };
            let (p3, k3) = if span + 2 < k {
                (w[span + 2][j], knots[span + 2])
            } else {
                (
                    p2 + (p2 - p1),
                    knots[span + 1] + (knots[span + 1] - knots[span]),
                )
            };
            barry_goldman([p0, p1, p2, p3], [k0, knots[span], knots[span + 1], k3], t)
        })
        .collect()
}

/// Sections at fractions `t` along a path; rings interpolated in 2D and
/// placed in the path's frames.
fn along_path(
    sections: &[LoftSection],
    rings2: &mut [Vec<Vec2>],
    apex: &[bool],
    o: &LoftOpts,
    steps: u32,
) -> Result<Rings, String> {
    let k = sections.len();
    if let Some(i) = sections
        .iter()
        .position(|s| s.rotation.is_some() || s.at != [0.0; 3])
    {
        return Err(format!(
            "section {i}: on a path, place sections with `t` (0..1 along it), not `at` / `y` / `rotation`"
        ));
    }
    let ts: Vec<f32> = sections
        .iter()
        .enumerate()
        .map(|(i, s)| s.t.unwrap_or(i as f32 / (k - 1) as f32))
        .collect();
    if ts.iter().any(|t| !(0.0..=1.0).contains(t)) || ts.windows(2).any(|w| w[1] <= w[0]) {
        return Err(format!(
            "section t values must rise strictly within 0..1, got {ts:?}"
        ));
    }
    let pts: Vec<Vec3> = sample_path(o.path, false, o.smooth, 0)
        .iter()
        .map(|p| Vec3::from(*p))
        .collect();
    if pts.len() < 2 || pts.windows(2).any(|w| (w[1] - w[0]).length() < 1e-9) {
        return Err("path needs at least 2 distinct points, none repeated".into());
    }
    let lens = arc_lengths(&pts, false);
    let total = lens[lens.len() - 1];
    let fr = frames(&pts, false);
    for i in 1..k {
        if !apex[i] && !apex[i - 1] {
            let a = normalise2(&rings2[i - 1]);
            let b = normalise2(&rings2[i]);
            let s = best_shift(&a, &b, |x, y| (x - y).length_squared());
            rings2[i].rotate_left(s);
        }
    }
    // Arc lengths to put a ring at: every section, every path point between,
    // and `steps` per span.
    let mut at: Vec<f32> = ts.iter().map(|t| t * total).collect();
    at.extend(
        lens.iter()
            .copied()
            .filter(|l| *l > ts[0] * total && *l < ts[k - 1] * total),
    );
    for span in 0..k - 1 {
        for st in 1..steps {
            at.push((ts[span] + (ts[span + 1] - ts[span]) * st as f32 / steps as f32) * total);
        }
    }
    at.sort_by(f32::total_cmp);
    at.dedup_by(|a, b| (*a - *b).abs() < total * 1e-5);
    let m = rings2[0].len();
    let mut out = Vec::with_capacity(at.len());
    for s in &at {
        let u = s / total;
        let span = ts.partition_point(|t| *t <= u).clamp(1, k - 1) - 1;
        let f = ((u - ts[span]) / (ts[span + 1] - ts[span])).clamp(0.0, 1.0);
        let r = interp(rings2, &ts, span, f, o.smooth, m);
        let (p, _, nrm, bin) = frame_at(&pts, &fr, &lens, false, *s);
        out.push(r.iter().map(|q| p + nrm * q.x + bin * q.y).collect());
    }
    let (_, t0, _, _) = frame_at(&pts, &fr, &lens, false, ts[0] * total);
    let (_, t1, _, _) = frame_at(&pts, &fr, &lens, false, ts[k - 1] * total);
    Ok((out, (t0, t1)))
}

/// Walls between consecutive rings plus end caps (from the 2D rings) or
/// apexes, oriented outward and proved closed by Manifold.
fn skin(
    name: &str,
    rings: &[Vec<Vec3>],
    first: Option<&[Vec2]>,
    last: Option<&[Vec2]>,
    (d0, d1): (Vec3, Vec3),
) -> Result<Part, String> {
    let r = rings.len();
    let m = rings[0].len();
    let mut part = Part::new(name);
    // Apex rings collapse to one vertex.
    let mut base = Vec::with_capacity(r);
    for (i, ring) in rings.iter().enumerate() {
        base.push(part.verts.len() as u32);
        let is_apex = (i == 0 && first.is_none()) || (i == r - 1 && last.is_none());
        if is_apex {
            let c = ring.iter().copied().sum::<Vec3>() / m as f32;
            part.verts.push(c.to_array());
        } else {
            part.verts.extend(ring.iter().map(|v| v.to_array()));
        }
    }
    let solo = |i: usize| (i == 0 && first.is_none()) || (i == r - 1 && last.is_none());
    let idx = |i: usize, j: usize| {
        if solo(i) {
            base[i]
        } else {
            base[i] + (j % m) as u32
        }
    };
    // Orientation of the walls: a ring CCW seen from the travel direction
    // gives outward quads in this order; check on a full ring.
    let probe = if first.is_some() { 0 } else { 1.min(r - 1) };
    let ring = &rings[probe];
    let c = ring.iter().copied().sum::<Vec3>() / m as f32;
    let area: Vec3 = (0..m)
        .map(|j| (ring[j] - c).cross(ring[(j + 1) % m] - c))
        .sum();
    let flip = area.dot(d0) < 0.0;
    let tri = |part: &mut Part, a: u32, b: u32, c: u32| {
        if a == b || b == c || a == c {
            return;
        }
        if flip {
            part.indices.extend([a, c, b]);
        } else {
            part.indices.extend([a, b, c]);
        }
    };
    for i in 0..r - 1 {
        for j in 0..m {
            let (p0, p1, p2, p3) = (idx(i, j), idx(i, j + 1), idx(i + 1, j + 1), idx(i + 1, j));
            tri(&mut part, p0, p1, p2);
            tri(&mut part, p0, p2, p3);
        }
    }
    // Caps, each facing away from the loft.
    for (ring2, i, out) in [(first, 0, -d0), (last, r - 1, d1)] {
        let Some(ring2) = ring2 else { continue };
        // Triangulate counter-clockwise, then map back to the ring's order.
        let ccw = signed_area(ring2) > 0.0;
        let mut poly: Vec<[f64; 2]> = ring2.iter().map(|p| [p.x as f64, p.y as f64]).collect();
        if !ccw {
            reverse_keep_first(&mut poly);
        }
        let mut tris = manifold_csg::triangulate_polygons(std::slice::from_ref(&poly), 1e-9)
            .ok_or_else(|| "could not triangulate an end profile".to_string())?;
        if !ccw {
            let back = |k: u32| (m as u32 - k) % m as u32;
            for t in &mut tris {
                *t = t.map(back);
            }
        }
        let v = |k: u32| Vec3::from(part.verts[(base[i] + k) as usize]);
        let nsum: Vec3 = tris
            .iter()
            .map(|t| (v(t[1]) - v(t[0])).cross(v(t[2]) - v(t[0])))
            .sum();
        let rev = nsum.dot(out) < 0.0;
        for t in &tris {
            let [a, b, c] = t.map(|k| base[i] + k);
            if rev {
                part.indices.extend([a, c, b]);
            } else {
                part.indices.extend([a, b, c]);
            }
        }
    }
    let man = crate::csg::part_to_manifold(&part)
        .map_err(|e| format!("{e} (do sections cross each other, or the path itself?)"))?;
    let out = crate::csg::manifold_to_part(&man, name, crate::Material::default());
    if out.indices.is_empty() {
        return Err("the skin came out empty".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn circle(r: f32, n: usize) -> Vec<[f32; 2]> {
        (0..n)
            .map(|i| {
                let a = i as f32 / n as f32 * std::f32::consts::TAU;
                [r * a.cos(), r * a.sin()]
            })
            .collect()
    }

    fn sec(profile: Vec<[f32; 2]>, y: f32) -> LoftSection {
        LoftSection {
            profile,
            at: [0.0, y, 0.0],
            t: None,
            rotation: None,
            scale: [1.0, 1.0],
        }
    }

    fn opts(path: &[[f32; 3]], smooth: bool) -> LoftOpts<'_> {
        LoftOpts {
            path,
            smooth,
            samples: 0,
            steps: 0,
            round: 0.0,
        }
    }

    fn bounds(p: &Part) -> (Vec3, Vec3) {
        p.verts.iter().fold((Vec3::MAX, Vec3::MIN), |(lo, hi), v| {
            (lo.min(Vec3::from(*v)), hi.max(Vec3::from(*v)))
        })
    }

    fn volume(p: &Part) -> f32 {
        p.indices
            .as_chunks::<3>()
            .0
            .iter()
            .map(|t| {
                let [a, b, c] = t.map(|i| Vec3::from(p.verts[i as usize]));
                a.dot(b.cross(c)) / 6.0
            })
            .sum()
    }

    #[test]
    fn circle_to_square_is_closed_outward_with_the_right_bounds() {
        let sq = vec![[-0.5, -0.5], [0.5, -0.5], [0.5, 0.5], [-0.5, 0.5]];
        let p = loft(
            "cs",
            &[sec(circle(0.3, 48), 0.0), sec(sq, 1.0)],
            &opts(&[], false),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let (lo, hi) = bounds(&p);
        assert!(
            (lo - Vec3::new(-0.5, 0.0, -0.5)).abs().max_element() < 1e-4,
            "{lo}"
        );
        assert!(
            (hi - Vec3::new(0.5, 1.0, 0.5)).abs().max_element() < 1e-4,
            "{hi}"
        );
        // Outward: positive volume between the circle's and the square's.
        let v = volume(&p);
        assert!(v > 0.28 && v < 1.0, "volume {v}");
        // Closed: Manifold accepted it again.
        assert!(crate::csg::part_to_manifold(&p).is_ok());
    }

    #[test]
    fn downward_and_smooth_lofts_stay_outward() {
        let p = loft(
            "b",
            &[
                sec(circle(0.04, 32), 0.3),
                sec(circle(0.04, 32), 0.15),
                sec(circle(0.015, 32), 0.0),
            ],
            &opts(&[], true),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(volume(&p) > 0.0);
        let (lo, hi) = bounds(&p);
        assert!(lo.y.abs() < 1e-5 && (hi.y - 0.3).abs() < 1e-5);
    }

    #[test]
    fn a_section_scaled_to_zero_is_an_apex() {
        let mut tip = sec(circle(0.1, 16), 1.0);
        tip.scale = [0.0, 0.0];
        let p = loft("cone", &[sec(circle(0.1, 16), 0.0), tip], &opts(&[], false))
            .unwrap_or_else(|e| panic!("{e}"));
        let v = volume(&p);
        let cone = std::f32::consts::PI * 0.01 / 3.0;
        assert!((v - cone).abs() < cone * 0.05, "{v} vs {cone}");
    }

    #[test]
    fn loft_along_a_path_follows_it() {
        let path = [[0.0, 0.0, 0.0], [1.0, 0.5, 0.0], [2.0, 0.0, 0.0]];
        let mut a = sec(circle(0.1, 24), 0.0);
        let mut b = sec(circle(0.05, 24), 0.0);
        a.at = [0.0; 3];
        b.at = [0.0; 3];
        let p = loft("horn", &[a, b], &opts(&path, true)).unwrap_or_else(|e| panic!("{e}"));
        assert!(volume(&p) > 0.0);
        let (lo, hi) = bounds(&p);
        assert!(lo.x < 0.0 && hi.x > 2.0 && hi.y > 0.5, "{lo} {hi}");
    }

    #[test]
    fn smooth_path_passes_through_its_control_points() {
        let ctrl = [
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [2.0, 0.0, 1.0],
            [3.0, 1.0, 1.0],
        ];
        let s = sample_path(&ctrl, false, true, 0);
        assert!(s.len() > 12, "{} samples", s.len());
        for c in &ctrl {
            assert!(
                s.iter()
                    .any(|p| (Vec3::from(*p) - Vec3::from(*c)).length() < 1e-5),
                "{c:?} not on the curve"
            );
        }
        let closed = sample_path(&ctrl, true, true, 8);
        assert_eq!(closed.len(), 32);
    }

    #[test]
    fn placements_count_spacing_and_orientation() {
        let path = [[0.0, 0.0, 0.0], [10.0, 0.0, 0.0]];
        let p = placements(&path, false, 0, 2.0, 0.0).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(p.len(), 6);
        assert!((p[1].0.x - 2.0).abs() < 1e-5 && (p[5].0.x - 10.0).abs() < 1e-5);
        // +x along the tangent, +y stays up on a level path.
        let path = [[0.0, 0.0, 0.0], [0.0, 0.0, 4.0]];
        let p = placements(&path, false, 3, 0.0, 90.0).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(p.len(), 3);
        assert!((p[0].1 * Vec3::X - Vec3::Z).length() < 1e-5);
        assert!((p[0].1 * Vec3::Y - Vec3::Y).length() < 1e-5);
        // Copy 1 rolled 90 degrees about the tangent: its up is sideways.
        assert!((p[1].1 * Vec3::Y).y.abs() < 1e-5);
        assert!((p[1].1 * Vec3::X - Vec3::Z).length() < 1e-5);
        // Closed: count = gaps, no copy twice at the seam.
        let sq = [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 0.0, 1.0],
            [0.0, 0.0, 1.0],
        ];
        let p = placements(&sq, true, 0, 0.5, 0.0).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(p.len(), 8);
    }
}
