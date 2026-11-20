//! Shoe volumes (sneakers, boots, flats) around a body's feet.
//!
//! Each foot is measured in its own frame (forward = ankle -> ball in plan,
//! lateral, up) at stations heel to toe: the widest lateral extent and the
//! highest point of the foot below the collar. The upper is lofted through
//! those stations as rounded superellipse sections grown by the shoe's
//! allowance (so the toe box bridges every toe), capped round at heel and
//! toe; a sole slab under it is wider than the upper (the tread edge),
//! thicker at the heel and lifted toward the toe (toe spring). Boots add a
//! shaft around the lower leg. Every vertex takes its skin weights from the
//! nearest foot vertex (toe bones moved onto the ball, so the shoe bends at
//! the ball like a real sole and never curls at the toes).

use super::{Mesh, Welded, Zone};
use crate::Part;
use bevy::math::{Vec2, Vec3};

struct Style {
    /// Collar height over the ankle joint (negative = below it).
    collar: f32,
    /// Sole thickness at the forefoot and extra at the heel.
    sole: f32,
    heel: f32,
    /// Allowance around the foot (toe box, sides) and over its top.
    side: f32,
    top: f32,
    /// Tread edge beyond the upper.
    tread: f32,
    toe_spring: f32,
    /// Upper height cap over the sole behind the toe box (flats: a low vamp).
    vamp: Option<f32>,
    /// Boot shaft top over the ankle joint.
    shaft: Option<f32>,
    /// How far the tongue rises over the collar at the instep.
    tongue: f32,
}

fn style(kind: &str) -> Style {
    match kind {
        "boots" => Style {
            collar: 0.01,
            sole: 0.022,
            heel: 0.014,
            side: 0.011,
            top: 0.012,
            tread: 0.005,
            toe_spring: 0.008,
            vamp: None,
            shaft: Some(0.15),
            tongue: 0.0,
        },
        "flats" => Style {
            collar: -0.035,
            sole: 0.008,
            heel: 0.006,
            side: 0.004,
            top: 0.005,
            tread: 0.001,
            toe_spring: 0.004,
            vamp: Some(0.03),
            shaft: None,
            tongue: 0.0,
        },
        // Sneakers.
        _ => Style {
            collar: 0.012,
            sole: 0.024,
            heel: 0.008,
            side: 0.009,
            top: 0.011,
            tread: 0.005,
            toe_spring: 0.012,
            vamp: None,
            shaft: None,
            tongue: 0.045,
        },
    }
}

const STATIONS: usize = 30;
const RING: usize = 28;

/// One side's shoe (side +1 = left, +x).
#[allow(clippy::too_many_arguments)]
fn foot(
    out: &mut Mesh,
    covers: &mut Vec<u32>,
    body: &Part,
    w: &Welded,
    zw: &[[f32; 8]],
    st: &Style,
    sfx: &str,
    side: f32,
) -> Result<(), String> {
    let joint = |n: &str| {
        body.joint(&format!("{n}{sfx}"))
            .ok_or_else(|| format!("shoes: `{}` has no joint `{n}{sfx}`", body.name))
    };
    let ankle = joint("ankle")?;
    let ball = joint("ball")?;
    let collar = ankle.y + st.collar;
    let fwd = Vec3::new(ball.x - ankle.x, 0.0, ball.z - ankle.z).normalize_or(Vec3::Z);
    let lat = Vec3::Y.cross(fwd);
    let feet: Vec<usize> = (0..w.pos.len())
        .filter(|g| {
            let foot = zw[*g][Zone::Foot as usize];
            let p = w.pos[*g];
            p.x * side > 0.0 && foot > 0.5 && p.y < collar + st.tongue + 0.03
        })
        .collect();
    if feet.len() < 20 {
        return Err(format!("shoes: `{}` has no foot{sfx} surface", body.name));
    }
    let local = |p: Vec3| {
        let d = p - ankle;
        (d.dot(fwd), d.dot(lat), p.y)
    };
    let (umin, umax) = feet.iter().fold((f32::MAX, f32::MIN), |(a, b), g| {
        let u = local(w.pos[*g]).0;
        (a.min(u), b.max(u))
    });
    let ball_u = local(ball).0;
    let u0 = umin - st.side * 0.6;
    let u1 = umax + st.side;
    // The collar rises over the instep into the tongue.
    let collar_at = |u: f32| {
        let a = u0 + (ball_u - u0) * 0.3;
        let b = ball_u - 0.035;
        let t = ((u - a) / 0.03).clamp(0.0, 1.0) * ((b - u) / 0.04).clamp(0.0, 1.0);
        collar + st.tongue * t * t * (3.0 - 2.0 * t)
    };
    // Per station: lateral min / max and top height of the foot below the
    // collar.
    let mut prof: Vec<Option<[f32; 3]>> = vec![None; STATIONS];
    let du = (u1 - u0) / (STATIONS - 1) as f32;
    for (k, pr) in prof.iter_mut().enumerate() {
        let u = u0 + du * k as f32;
        let mut acc: Option<[f32; 3]> = None;
        for g in &feet {
            let (pu, pv, py) = local(w.pos[*g]);
            if (pu - u).abs() > du.max(0.012) || py > collar_at(pu) {
                continue;
            }
            let a = acc.get_or_insert([pv, pv, py]);
            a[0] = a[0].min(pv);
            a[1] = a[1].max(pv);
            a[2] = a[2].max(py);
        }
        *pr = acc;
    }
    for k in 0..STATIONS {
        if prof[k].is_none() {
            prof[k] = (1..STATIONS).find_map(|d| {
                [k.checked_sub(d), Some(k + d)]
                    .into_iter()
                    .flatten()
                    .find_map(|kk| prof.get(kk).copied().flatten())
            });
        }
    }
    let mut prof: Vec<[f32; 3]> = prof.into_iter().flatten().collect();
    if prof.len() != STATIONS {
        return Err("shoes: foot profile failed".into());
    }
    // A smooth plan outline: running max then smoothing (bridges toes).
    for _ in 0..3 {
        let pr = prof.clone();
        for k in 0..STATIONS {
            let (a, b) = (k.saturating_sub(1), (k + 1).min(STATIONS - 1));
            prof[k][0] = pr[a][0].min(pr[b][0]).min(pr[k][0]) * 0.5 + pr[k][0] * 0.5;
            prof[k][1] = pr[a][1].max(pr[b][1]).max(pr[k][1]) * 0.5 + pr[k][1] * 0.5;
            prof[k][2] = (pr[a][2] + 2.0 * pr[k][2] + pr[b][2]) / 4.0;
        }
    }
    let spring = |u: f32| {
        st.toe_spring
            * ((u - ball_u) / (u1 - ball_u).max(0.01))
                .clamp(0.0, 1.0)
                .powi(2)
    };
    let sole_h =
        |u: f32| st.sole + st.heel * (1.0 - ((u - u0) / (ball_u - u0).max(0.01)).clamp(0.0, 1.0));
    // End rounding: heel and toe caps shrink the section.
    let round = |k: usize| {
        let t = k as f32 / (STATIONS - 1) as f32;
        let toe = ((1.0 - t) / 0.16).min(1.0);
        let heel = (t / 0.07).min(1.0);
        let f = |x: f32| (1.0 - (1.0 - x).powi(2)).max(0.0).sqrt().max(0.18);
        (f(toe), f(heel))
    };
    let at = |u: f32, v: f32, y: f32| ankle + fwd * u + lat * v + Vec3::Y * (y - ankle.y);
    // Nearest foot vertex for weights.
    let src_of = |p: Vec3| {
        feet.iter()
            .copied()
            .min_by(|a, b| {
                (w.pos[*a] - p)
                    .length_squared()
                    .total_cmp(&(w.pos[*b] - p).length_squared())
            })
            .map(|g| (g as u32, g as u32, 0.0))
    };
    let loft = |out: &mut Mesh, sections: &dyn Fn(usize, usize) -> Vec3| {
        let base = out.pos.len() as u32;
        for k in 0..STATIONS {
            for j in 0..RING {
                let p = sections(k, j);
                out.pos.push(p);
                out.src.push(src_of(p));
            }
        }
        let ring = |k: usize, j: usize| base + (k * RING + j % RING) as u32;
        for k in 0..STATIONS - 1 {
            for j in 0..RING {
                out.tris.push([ring(k, j), ring(k, j + 1), ring(k + 1, j)]);
                out.tris
                    .push([ring(k, j + 1), ring(k + 1, j + 1), ring(k + 1, j)]);
            }
        }
        // End caps.
        for (k, flip) in [(0usize, true), (STATIONS - 1, false)] {
            let c: Vec3 = (0..RING)
                .map(|j| out.pos[ring(k, j) as usize])
                .sum::<Vec3>()
                / RING as f32;
            let ci = out.pos.len() as u32;
            out.pos.push(c);
            out.src.push(src_of(c));
            for j in 0..RING {
                let (a, b) = (ring(k, j), ring(k, j + 1));
                out.tris.push(if flip { [ci, b, a] } else { [ci, a, b] });
            }
        }
        // Outward winding: check the mid station's first face.
        let mid = STATIONS / 2;
        let (a, b, c) = (ring(mid, 0), ring(mid, 1), ring(mid + 1, 0));
        let [pa, pb, pc] = [a, b, c].map(|i| out.pos[i as usize]);
        let centre: Vec3 = (0..RING)
            .map(|j| out.pos[ring(mid, j) as usize])
            .sum::<Vec3>()
            / RING as f32;
        let n = (pb - pa).cross(pc - pa);
        if n.dot(pa - centre) < 0.0 {
            let t0 = out.tris.len() - (2 * (STATIONS - 1) * RING + 2 * RING);
            for t in &mut out.tris[t0..] {
                t.swap(1, 2);
            }
        }
    };
    let ang = |j: usize| j as f32 / RING as f32 * std::f32::consts::TAU;
    let se = |x: f32, n: f32| x.signum() * x.abs().powf(2.0 / n);
    // Upper: a flat-bottomed superellipse from the sole to over the foot.
    loft(out, &|k, j| {
        let u = u0 + du * k as f32;
        let [vmin, vmax, top] = prof[k];
        let (rt, rh) = round(k);
        let r = rt.min(rh);
        let base = sole_h(u) - 0.004;
        let mut h = top + st.top;
        if let Some(v) = st.vamp
            && u < ball_u - 0.01
        {
            h = h.min(sole_h(u) + v);
        }
        // A toe box with room over the toes.
        let room = if st.vamp.is_some() { 0.02 } else { 0.045 };
        h = h.min(collar_at(u)).max(base + room);
        let cv = (vmin + vmax) * 0.5;
        let rv = ((vmax - vmin) * 0.5 + st.side) * r;
        let a = ang(j);
        let (c, s) = (a.cos(), a.sin());
        let v = cv + rv * se(c, 3.0);
        let y = if s > 0.0 {
            base + (h - base) * rt.clamp(0.4, 1.0) * se(s, 2.6)
        } else {
            base + 0.003 * s
        };
        at(u, v, y + spring(u))
    });
    // Sole: a rounded slab, wider than the upper.
    loft(out, &|k, j| {
        let u = u0 + du * k as f32;
        let [vmin, vmax, _] = prof[k];
        let (rt, rh) = round(k);
        let r = rt.min(rh);
        let hh = sole_h(u);
        let cv = (vmin + vmax) * 0.5;
        let rv = ((vmax - vmin) * 0.5 + st.side) * r + st.tread;
        let a = ang(j);
        let (c, s) = (a.cos(), a.sin());
        let v = cv + rv * se(c, 8.0);
        let y = hh * 0.5 + hh * 0.5 * se(s, 8.0);
        // A rocker at the heel: the back edge rounds up so a heel strike
        // rolls instead of digging in.
        let rock = 0.007 * ((u0 + 0.035 - u) / 0.035).clamp(0.0, 1.0).powi(2);
        let lift = (spring(u) + rock) * (1.0 - y / hh.max(1e-3)) + spring(u) * (y / hh.max(1e-3));
        at(u, v, y + lift + 0.0015)
    });
    // Boot shaft: hull rings of the lower leg, open at the top.
    if let Some(top) = st.shaft {
        let knee = joint("knee")?;
        let y0 = collar - 0.04;
        let y1 = ankle.y + top;
        let n = ((y1 - y0) / 0.015).ceil().max(2.0) as usize;
        let seg = 32usize;
        let base = out.pos.len() as u32;
        let mut rings: Vec<(Vec2, Vec<f32>)> = Vec::new();
        for k in 0..=n {
            let y = y0 + (y1 - y0) * k as f32 / n as f32;
            // Around the shin axis (knee -> ankle) at this height.
            let t = ((y - ankle.y) / (knee.y - ankle.y)).clamp(0.0, 1.0);
            let ax = ankle.lerp(knee, t);
            let c = Vec2::new(ax.x, ax.z);
            let mut r = vec![0.0f32; seg];
            for (g, p) in w.pos.iter().enumerate() {
                let z = zw[g];
                if p.x * side <= 0.0
                    || (p.y - y).abs() > 0.012
                    || z[Zone::Leg as usize] + z[Zone::Foot as usize] < 0.5
                {
                    continue;
                }
                let d = Vec2::new(p.x, p.z) - c;
                if d.length() > 0.09 {
                    continue;
                }
                let a = d.y.atan2(d.x).rem_euclid(std::f32::consts::TAU);
                let j = ((a / std::f32::consts::TAU * seg as f32).round() as usize) % seg;
                r[j] = r[j].max(d.length());
            }
            if r.iter().all(|x| *x <= 0.0) {
                if let Some(l) = rings.last().cloned() {
                    rings.push(l);
                }
                continue;
            }
            for _ in 0..seg {
                if r.iter().all(|x| *x > 0.0) {
                    break;
                }
                let pr = r.clone();
                for j in 0..seg {
                    if pr[j] <= 0.0 {
                        r[j] = pr[(j + seg - 1) % seg].max(pr[(j + 1) % seg]);
                    }
                }
            }
            // Convex-ish: no dent deeper than the neighbours' mean.
            for _ in 0..4 {
                let pr = r.clone();
                for j in 0..seg {
                    let m = (pr[(j + seg - 1) % seg] + pr[(j + 1) % seg]) * 0.5;
                    r[j] = pr[j].max(m);
                }
            }
            let r: Vec<f32> = r.iter().map(|x| x + st.side + 0.004).collect();
            rings.push((c, r));
        }
        if rings.len() >= 2 {
            for _ in 0..3 {
                let pr = rings.clone();
                for k in 0..rings.len() {
                    let (a, b) = (k.saturating_sub(1), (k + 1).min(rings.len() - 1));
                    for j in 0..seg {
                        let jm = (j + seg - 1) % seg;
                        let jp = (j + 1) % seg;
                        rings[k].1[j] = (pr[a].1[j]
                            + pr[b].1[j]
                            + pr[k].1[jm]
                            + pr[k].1[jp]
                            + 4.0 * pr[k].1[j])
                            / 8.0;
                    }
                }
            }
            let mut pos = Vec::new();
            let nr = rings.len();
            for (k, (c, r)) in rings.iter().enumerate() {
                let y = y0 + (y1 - y0) * k as f32 / (nr - 1) as f32;
                for (j, rj) in r.iter().enumerate() {
                    let a = j as f32 / seg as f32 * std::f32::consts::TAU;
                    pos.push(Vec3::new(c.x + a.cos() * rj, y, c.y + a.sin() * rj));
                }
            }
            let mut tris = Vec::new();
            for k in 0..nr - 1 {
                for j in 0..seg {
                    let a = (k * seg + j) as u32;
                    let b = (k * seg + (j + 1) % seg) as u32;
                    tris.push([a, a + seg as u32, b]);
                    tris.push([b, a + seg as u32, b + seg as u32]);
                }
            }
            let nn = super::vertex_normals(&pos, &tris);
            let c0 = rings[0].0;
            if (pos[0] - Vec3::new(c0.x, y0, c0.y)).dot(nn[0]) < 0.0 {
                for t in &mut tris {
                    t.swap(1, 2);
                }
            }
            let src: Vec<(u32, u32, f32)> = pos
                .iter()
                .map(|p| {
                    let g = (0..w.pos.len())
                        .filter(|g| w.pos[*g].x * side > 0.0)
                        .min_by(|a, b| {
                            (w.pos[*a] - *p)
                                .length_squared()
                                .total_cmp(&(w.pos[*b] - *p).length_squared())
                        })
                        .unwrap_or(0) as u32;
                    (g, g, 0.0)
                })
                .collect();
            super::emit(out, &pos, &tris, &vec![0.004; pos.len()], Some(&src));
            let _ = base;
        }
    }
    // Hide the foot (and, in boots, the leg) inside.
    let hide_top = st.shaft.map_or(collar - 0.008, |t| ankle.y + t - 0.015);
    let inside = |g: u32| {
        let p = w.pos[g as usize];
        if p.x * side <= 0.0 || p.y > hide_top {
            return false;
        }
        let (u, v, y) = local(p);
        if u < u0 || u > u1 {
            return st.shaft.is_some() && p.y > collar - 0.03;
        }
        let k = (((u - u0) / du).round() as usize).min(STATIONS - 1);
        let [vmin, vmax, top] = prof[k];
        let mut h = top + st.top;
        if let Some(vv) = st.vamp
            && u < ball_u - 0.01
        {
            h = h.min(sole_h(u) + vv);
        }
        let shaft = st.shaft.is_some() && p.y > collar - 0.03;
        shaft || (v > vmin - 0.01 && v < vmax + 0.01 && y < h.min(collar_at(u)) - 0.006)
    };
    for (ti, t) in w.tris.iter().enumerate() {
        if t.iter().all(|g| inside(*g)) {
            covers.push(w.raw_tri[ti]);
        }
    }
    Ok(())
}

/// Both shoes on `body`; mesh vertices carry their weight source.
pub(super) fn build(kind: &str, body: &Part, w: &Welded) -> Result<(Mesh, Vec<u32>), String> {
    let st = style(kind);
    let zw: Vec<[f32; 8]> = (0..w.pos.len())
        .map(|g| super::zone_weights(body, w, g))
        .collect();
    let mut mesh = Mesh::default();
    let mut covers = Vec::new();
    for (sfx, side) in [("_l", 1.0f32), ("_r", -1.0)] {
        foot(&mut mesh, &mut covers, body, w, &zw, &st, sfx, side)?;
    }
    Ok((mesh, covers))
}
