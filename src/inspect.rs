//! Numeric "senses" over a model, beyond pass/fail checks:
//! - [`measure`]: gap, contact or overlap between two parts (or a part and the ground);
//! - [`section`]: a planar cut through the visible parts, as outlines, areas and a plot;
//! - [`mass`]: volume, mass, centre of mass and whether the model tips over.
//!
//! All work in world space (part transforms applied) and metres.

use crate::raster::{Canvas, fmt_len, id_color_u8};
use crate::{Model, ModelerError, Part, Vec3};
use manifold_csg::Manifold;
use std::collections::BTreeMap;

fn part<'a>(model: &'a Model, name: &str) -> Result<&'a Part, ModelerError> {
    model
        .part(name)
        .ok_or_else(|| ModelerError::NoPart(name.to_string()))
}

fn visible(model: &Model) -> impl Iterator<Item = &Part> {
    model
        .parts
        .iter()
        .filter(|p| p.visible && !p.verts.is_empty() && !p.indices.is_empty())
        .map(|p| &**p)
}

fn fmt_m(v: f64) -> String {
    if v.abs() >= 0.01 || v == 0.0 {
        format!("{v:.4} m")
    } else {
        format!("{v:.3e} m")
    }
}

fn fmt_m3(v: f64) -> String {
    if v.abs() >= 0.01 {
        format!("{v:.4} m³")
    } else {
        format!("{v:.3e} m³")
    }
}

// ---------------------------------------------------------------------------
// measure
// ---------------------------------------------------------------------------

/// One measurement: `[a, b]` = how two parts relate; `[a]` = height above the
/// ground. Text plus JSON.
pub fn measure(
    model: &Model,
    pair: &[String],
) -> Result<(String, serde_json::Value), ModelerError> {
    match pair {
        [a] => {
            let p = part(model, a)?;
            let (lo, hi) = p.bounds();
            let gap = lo.y as f64;
            let rel = if gap.abs() < 1e-5 {
                "rests on the ground".to_string()
            } else if gap > 0.0 {
                format!("floats {} above the ground", fmt_m(gap))
            } else {
                format!("sinks {} below the ground", fmt_m(-gap))
            };
            Ok((
                format!("`{a}` {rel} (y {:.4}..{:.4})", lo.y, hi.y),
                serde_json::json!({"a": a, "ground_gap": gap, "min_y": lo.y, "max_y": hi.y}),
            ))
        }
        [a, b] => {
            let (pa, pb) = (part(model, a)?, part(model, b)?);
            let (alo, ahi) = pa.bounds();
            let (blo, bhi) = pb.bounds();
            // Per-axis gap between the boxes (negative = the boxes overlap).
            let axis_gap: Vec<f64> = (0..3)
                .map(|k| (blo[k] - ahi[k]).max(alo[k] - bhi[k]) as f64)
                .collect();
            let centre = ((alo + ahi) * 0.5).distance((blo + bhi) * 0.5) as f64;
            let diag = (ahi.max(bhi) - alo.min(blo)).length() as f64;
            let (ma, mb) = (
                crate::csg::part_to_manifold(pa),
                crate::csg::part_to_manifold(pb),
            );
            let (relation, gap, overlap) = match (ma, mb) {
                (Ok(ma), Ok(mb)) => {
                    let inter = ma.intersection(&mb);
                    let ov = inter.volume().abs();
                    let small = ma.volume().abs().min(mb.volume().abs()).max(1e-30);
                    if ov > small * 1e-6 {
                        let depth = inter
                            .bounding_box()
                            .map(|bb| {
                                let (lo, hi) = (bb.min(), bb.max());
                                [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]]
                            })
                            .unwrap_or([0.0; 3]);
                        let pen = depth.iter().cloned().fold(f64::MAX, f64::min);
                        (
                            format!(
                                "overlap: {} shared ({:.1}% of the smaller), interpenetrating ~{} (overlap box {} x {} x {})",
                                fmt_m3(ov),
                                ov / small * 100.0,
                                fmt_m(pen),
                                fmt_m(depth[0]),
                                fmt_m(depth[1]),
                                fmt_m(depth[2])
                            ),
                            -pen,
                            ov,
                        )
                    } else {
                        let search = diag.max(1e-3);
                        let g = ma.min_gap(&mb, search);
                        let tol = (diag * 1e-5).max(1e-7);
                        if g <= tol {
                            (
                                "touching (surfaces meet, no shared volume)".into(),
                                0.0,
                                0.0,
                            )
                        } else if g >= search {
                            (format!("apart by more than {}", fmt_m(search)), g, 0.0)
                        } else {
                            (
                                format!("apart: closest surfaces {} apart", fmt_m(g)),
                                g,
                                0.0,
                            )
                        }
                    }
                }
                // Open sheets: fall back to the boxes.
                _ => {
                    let g = axis_gap.iter().cloned().fold(f64::MIN, f64::max);
                    (
                        format!(
                            "box gap {} (one part is not a closed solid, so only bounds were compared)",
                            fmt_m(g)
                        ),
                        g,
                        0.0,
                    )
                }
            };
            let text = format!(
                "`{a}` vs `{b}`: {relation}; box gap x {} y {} z {} (negative = boxes overlap); centres {} apart",
                fmt_m(axis_gap[0]),
                fmt_m(axis_gap[1]),
                fmt_m(axis_gap[2]),
                fmt_m(centre)
            );
            Ok((
                text,
                serde_json::json!({
                    "a": a, "b": b, "gap": gap, "overlap_volume": overlap,
                    "box_gap": axis_gap, "centre_distance": centre, "relation": relation,
                }),
            ))
        }
        _ => Err(ModelerError::Invalid(
            "measure takes [a, b] (two parts) or [a] (a part vs the ground)".into(),
        )),
    }
}

// ---------------------------------------------------------------------------
// section
// ---------------------------------------------------------------------------

/// Maps world (x, y, z) to section-plane coordinates (u, v, w) with w along
/// the cut axis. Proper rotations, so winding (and solidity) is preserved;
/// (u, v) reads like the matching orthographic view: y = plan (+x right, -z
/// up), z = front (+x right, +y up), x = right side (-z right, +y up).
fn to_plane(axis: char, v: Vec3) -> Vec3 {
    match axis {
        'x' => Vec3::new(-v.z, v.y, v.x),
        'y' => Vec3::new(v.x, -v.z, v.y),
        _ => v,
    }
}

fn axis_names(axis: char) -> (&'static str, &'static str) {
    match axis {
        'x' => ("-z", "y"),
        'y' => ("x", "-z"),
        _ => ("x", "y"),
    }
}

/// Cuts every visible part (or `only`) with the plane `axis = at`. Returns
/// the report, JSON, and a PNG plot (each part's filled section in its own
/// colour, a scale bar and the axis directions).
pub fn section(
    model: &Model,
    axis: &str,
    at: f32,
    only: Option<&[String]>,
) -> Result<(String, serde_json::Value, Vec<u8>), ModelerError> {
    let axis = match axis.to_ascii_lowercase().as_str() {
        "x" => 'x',
        "y" => 'y',
        "z" => 'z',
        other => {
            return Err(ModelerError::Invalid(format!(
                "section axis must be x, y or z, got `{other}`"
            )));
        }
    };
    let mut cuts: Vec<(String, Vec<Vec<[f64; 2]>>, f64)> = Vec::new();
    let mut skipped = Vec::new();
    for p in visible(model) {
        if only.is_some_and(|o| !o.contains(&p.name)) {
            continue;
        }
        let (lo, hi) = p.bounds();
        let k = match axis {
            'x' => 0,
            'y' => 1,
            _ => 2,
        };
        if at < lo[k] || at > hi[k] {
            continue;
        }
        let mut world = p.clone();
        for i in 0..p.verts.len() {
            world.verts[i] = to_plane(axis, p.world_vert(i)).to_array();
        }
        world.translation = [0.0; 3];
        world.rotation = [0.0, 0.0, 0.0, 1.0];
        world.scale = [1.0; 3];
        let m = match crate::csg::part_to_manifold(&world) {
            Ok(m) => m,
            Err(_) => {
                skipped.push(p.name.clone());
                continue;
            }
        };
        let rings = m.slice_at_z(at as f64);
        if rings.is_empty() {
            continue;
        }
        let area = rings.iter().map(|r| ring_area(r)).sum::<f64>().abs();
        cuts.push((p.name.clone(), rings, area));
    }
    if let Some(o) = only
        && let Some(missing) = o.iter().find(|n| model.part(n).is_none())
    {
        return Err(ModelerError::NoPart(missing.clone()));
    }
    let (un, vn) = axis_names(axis);
    let mut text = format!(
        "section {axis} = {at}: {} part(s) cut (plot axes: {un} right, {vn} up)\n",
        cuts.len()
    );
    let mut json_parts = Vec::new();
    let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
    for (name, rings, area) in &cuts {
        let (mut plo, mut phi) = ([f64::MAX; 2], [f64::MIN; 2]);
        for r in rings {
            for p in r {
                for k in 0..2 {
                    plo[k] = plo[k].min(p[k]);
                    phi[k] = phi[k].max(p[k]);
                }
            }
        }
        for k in 0..2 {
            lo[k] = lo[k].min(plo[k]);
            hi[k] = hi[k].max(phi[k]);
        }
        let holes = rings.iter().filter(|r| ring_area(r) < 0.0).count();
        let thin = thinnest_wall(rings);
        text += &format!(
            "- `{name}`: area {:.6} m², {} outline(s){}, spans {un} {:.4}..{:.4}, {vn} {:.4}..{:.4}{}\n",
            area,
            rings.len() - holes,
            if holes > 0 {
                format!(" + {holes} hole(s)")
            } else {
                String::new()
            },
            plo[0],
            phi[0],
            plo[1],
            phi[1],
            thin.map(|t| format!(", thinnest wall ~{}", fmt_m(t)))
                .unwrap_or_default()
        );
        json_parts.push(serde_json::json!({
            "part": name, "area": area, "rings": rings.len(), "holes": holes,
            "min": plo, "max": phi, "thinnest_wall": thin,
        }));
    }
    if !skipped.is_empty() {
        text += &format!("not closed solids, skipped: {}\n", skipped.join(", "));
    }
    if cuts.is_empty() {
        text += "the plane misses every part\n";
    }
    let png = plot_section(&cuts, lo, hi, axis, at)?;
    Ok((
        text,
        serde_json::json!({"axis": axis.to_string(), "at": at, "parts": json_parts, "skipped": skipped}),
        png,
    ))
}

fn ring_area(r: &[[f64; 2]]) -> f64 {
    let mut a = 0.0;
    for k in 0..r.len() {
        let (p, q) = (r[k], r[(k + 1) % r.len()]);
        a += p[0] * q[1] - q[0] * p[1];
    }
    a * 0.5
}

/// A cheap wall-thickness estimate for sections with holes: the smallest
/// distance from a hole's vertices to an outer ring's edges. `None` without
/// holes (a solid section has no wall).
fn thinnest_wall(rings: &[Vec<[f64; 2]>]) -> Option<f64> {
    let (outer, holes): (Vec<_>, Vec<_>) = rings.iter().partition(|r| ring_area(r) >= 0.0);
    if holes.is_empty() {
        return None;
    }
    let seg = |p: [f64; 2], a: [f64; 2], b: [f64; 2]| {
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let l2 = dx * dx + dy * dy;
        let t = if l2 > 0.0 {
            (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / l2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        ((p[0] - a[0] - t * dx).powi(2) + (p[1] - a[1] - t * dy).powi(2)).sqrt()
    };
    let mut best = f64::MAX;
    for h in &holes {
        for p in h.iter() {
            for o in &outer {
                for k in 0..o.len() {
                    best = best.min(seg(*p, o[k], o[(k + 1) % o.len()]));
                }
            }
        }
    }
    (best < f64::MAX).then_some(best)
}

fn plot_section(
    cuts: &[(String, Vec<Vec<[f64; 2]>>, f64)],
    lo: [f64; 2],
    hi: [f64; 2],
    axis: char,
    at: f32,
) -> Result<Vec<u8>, ModelerError> {
    let (w, h) = (800usize, 600usize);
    let mut c = Canvas::new(w, h, [24, 26, 30, 255]);
    let (un, vn) = axis_names(axis);
    c.text(
        6,
        6,
        &format!("SECTION {axis} = {at}   {un} RIGHT, {vn} UP"),
        2,
    );
    if cuts.is_empty() {
        c.text(6, 40, "THE PLANE MISSES EVERY PART", 2);
        return c.to_png().map_err(ModelerError::Invalid);
    }
    let span = [(hi[0] - lo[0]).max(1e-6), (hi[1] - lo[1]).max(1e-6)];
    let (pad, top) = (40.0, 40.0);
    let s = ((w as f64 - 2.0 * pad) / span[0]).min((h as f64 - top - 2.0 * pad) / span[1]);
    let (ox, oy) = (
        (w as f64 - span[0] * s) * 0.5,
        top + (h as f64 - top - span[1] * s) * 0.5,
    );
    let to_px = |p: [f64; 2]| (ox + (p[0] - lo[0]) * s, oy + (hi[1] - p[1]) * s);
    for (i, (_, rings, _)) in cuts.iter().enumerate() {
        let px: Vec<Vec<(f64, f64)>> = rings
            .iter()
            .map(|r| r.iter().map(|p| to_px(*p)).collect())
            .collect();
        c.fill_rings(&px, id_color_u8(i));
        for r in &px {
            for k in 0..r.len() {
                c.line(r[k], r[(k + 1) % r.len()], [255, 255, 255, 255], 1);
            }
        }
    }
    // Legend.
    let mut y = h - 22;
    for (i, (name, _, _)) in cuts.iter().enumerate().rev() {
        let col = id_color_u8(i);
        for dy in 0..14 {
            for dx in 0..14 {
                c.set(6 + dx, y as i64 + dy, col);
            }
        }
        c.text(24, y, name, 2);
        if y < 60 {
            break;
        }
        y -= 20;
    }
    scale_bar(&mut c, s, w - 10, h - 14);
    c.to_png().map_err(ModelerError::Invalid)
}

/// Draws a 1-2-5 scale bar ending at (right, y) for `ppm` pixels per metre.
pub fn scale_bar(c: &mut Canvas, ppm: f64, right: usize, y: usize) {
    if !ppm.is_finite() || ppm <= 0.0 {
        return;
    }
    let target = 150.0 / ppm;
    let mag = 10f64.powf(target.log10().floor());
    let len = [1.0, 2.0, 5.0, 10.0]
        .iter()
        .map(|m| m * mag)
        .rfind(|l| *l <= target)
        .unwrap_or(mag);
    let px = len * ppm;
    let x0 = right as f64 - px;
    let white = [255, 255, 255, 255];
    c.line((x0, y as f64), (right as f64, y as f64), white, 3);
    c.line((x0, y as f64 - 6.0), (x0, y as f64 + 2.0), white, 2);
    c.line(
        (right as f64, y as f64 - 6.0),
        (right as f64, y as f64 + 2.0),
        white,
        2,
    );
    let label = fmt_len(len);
    let tw = Canvas::text_width(&label, 2);
    c.text(
        (right as f64 - px * 0.5 - tw as f64 * 0.5).max(0.0) as usize,
        y.saturating_sub(26),
        &label,
        2,
    );
}

// ---------------------------------------------------------------------------
// mass / stability
// ---------------------------------------------------------------------------

/// Volume and volume centroid of a part in world space (divergence theorem).
pub fn volume_centroid(p: &Part) -> (f64, [f64; 3]) {
    let (mut vol, mut c) = (0.0f64, [0.0f64; 3]);
    for t in p.indices.as_chunks::<3>().0 {
        let w = |i: u32| p.world_vert(i as usize).as_dvec3();
        let (a, b, d) = (w(t[0]), w(t[1]), w(t[2]));
        let v = a.dot(b.cross(d)) / 6.0;
        vol += v;
        let s = (a + b + d) * (v / 4.0);
        c[0] += s.x;
        c[1] += s.y;
        c[2] += s.z;
    }
    if vol.abs() > 1e-30 {
        c = [c[0] / vol, c[1] / vol, c[2] / vol];
    }
    (vol, c)
}

/// Mass report over the visible parts: per-part volume and mass (density
/// kg/m³ from `density[name]`, else `default_density`), the combined centre
/// of mass, and stability: the convex footprint where the model meets its
/// lowest level, whether the centre of mass sits over it, and the tilt (°)
/// it tolerates before tipping.
pub fn mass(
    model: &Model,
    density: &BTreeMap<String, f64>,
    default_density: f64,
) -> (String, serde_json::Value) {
    mass_of(model, None, density, default_density)
}

/// [`mass`] over just `only` (names, trailing `*` = prefix) when given:
/// weigh one object of a scene, and judge its own footprint.
pub fn mass_of(
    model: &Model,
    only: Option<&[String]>,
    density: &BTreeMap<String, f64>,
    default_density: f64,
) -> (String, serde_json::Value) {
    let picked = |p: &Part| {
        only.is_none_or(|o| {
            o.iter().any(|n| match n.strip_suffix('*') {
                Some(pre) => p.name.starts_with(pre),
                None => &p.name == n,
            })
        })
    };
    let mut text = String::new();
    let (mut total_m, mut total_v, mut com) = (0.0, 0.0, [0.0f64; 3]);
    let mut rows = Vec::new();
    let mut not_solid = Vec::new();
    for p in visible(model).filter(|p| picked(p)) {
        let (v, c) = volume_centroid(p);
        if v <= 0.0 || crate::verify::open_edge_count(p) > 0 {
            not_solid.push(p.name.clone());
            continue;
        }
        let d = density.get(&p.name).copied().unwrap_or(default_density);
        let m = v * d;
        total_m += m;
        total_v += v;
        for k in 0..3 {
            com[k] += c[k] * m;
        }
        rows.push(serde_json::json!({"part": p.name, "volume": v, "density": d, "mass": m, "centroid": c}));
        text += &format!("- `{}`: {} x {d} kg/m³ = {:.3} kg\n", p.name, fmt_m3(v), m);
    }
    if total_m <= 0.0 {
        return (
            "no closed visible parts to weigh".into(),
            serde_json::json!({"ok": false}),
        );
    }
    for c in &mut com {
        *c /= total_m;
    }
    let mut head = format!(
        "total {:.3} kg, {} ; centre of mass [{:.4}, {:.4}, {:.4}]\n",
        total_m,
        fmt_m3(total_v),
        com[0],
        com[1],
        com[2]
    );
    // Footprint: verts within a hair of the lowest point, convex hull in xz.
    let (mlo, mhi) = visible(model)
        .filter(|p| picked(p))
        .map(|p| p.bounds())
        .fold((Vec3::MAX, Vec3::MIN), |(a, b), (lo, hi)| {
            (a.min(lo), b.max(hi))
        });
    let tol = ((mhi - mlo).length() as f64 * 2e-4).max(1e-5);
    let floor = mlo.y as f64;
    let mut pts: Vec<[f64; 2]> = Vec::new();
    for p in visible(model).filter(|p| picked(p)) {
        for i in 0..p.verts.len() {
            let v = p.world_vert(i);
            if (v.y as f64) <= floor + tol {
                pts.push([v.x as f64, v.z as f64]);
            }
        }
    }
    let hull = convex_hull_2d(pts);
    let height = com[1] - floor;
    let stab = if hull.len() < 3 {
        head += "stability: the model rests on a point or an edge, so it will tip\n";
        serde_json::json!({"stable": false, "margin": 0.0, "tip_angle_deg": 0.0, "footprint": hull})
    } else {
        let margin = hull_margin(&hull, [com[0], com[2]]);
        let angle = margin.atan2(height.max(1e-9)).to_degrees();
        if margin > 0.0 {
            head += &format!(
                "stability: STABLE: the centre of mass is {} inside the footprint edge; it tips past ~{angle:.1}° of tilt ({} footprint corners, floor y = {floor:.4})\n",
                fmt_m(margin),
                hull.len()
            );
        } else {
            head += &format!(
                "stability: TIPS OVER: the centre of mass is {} outside the footprint (floor y = {floor:.4})\n",
                fmt_m(-margin)
            );
        }
        serde_json::json!({"stable": margin > 0.0, "margin": margin, "tip_angle_deg": angle, "footprint": hull})
    };
    if !not_solid.is_empty() {
        text += &format!(
            "not weighed (not closed solids): {}\n",
            not_solid.join(", ")
        );
    }
    (
        head + &text,
        serde_json::json!({"mass": total_m, "volume": total_v, "center_of_mass": com, "parts": rows, "stability": stab, "skipped": not_solid}),
    )
}

/// Monotone-chain convex hull, counter-clockwise.
fn convex_hull_2d(mut pts: Vec<[f64; 2]>) -> Vec<[f64; 2]> {
    pts.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    pts.dedup_by(|a, b| (a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9);
    if pts.len() < 3 {
        return pts;
    }
    let cross = |o: [f64; 2], a: [f64; 2], b: [f64; 2]| {
        (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
    };
    let mut h: Vec<[f64; 2]> = Vec::with_capacity(pts.len() * 2);
    for pass in 0..2 {
        let start = h.len();
        let iter: Box<dyn Iterator<Item = &[f64; 2]>> = if pass == 0 {
            Box::new(pts.iter())
        } else {
            Box::new(pts.iter().rev())
        };
        for p in iter {
            while h.len() >= start + 2 && cross(h[h.len() - 2], h[h.len() - 1], *p) <= 0.0 {
                h.pop();
            }
            h.push(*p);
        }
        h.pop();
    }
    h
}

/// Signed distance from `p` to the boundary of a CCW convex polygon:
/// positive inside.
fn hull_margin(hull: &[[f64; 2]], p: [f64; 2]) -> f64 {
    let mut best = f64::MAX;
    for k in 0..hull.len() {
        let (a, b) = (hull[k], hull[(k + 1) % hull.len()]);
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let len = (dx * dx + dy * dy).sqrt().max(1e-12);
        // Left of a CCW edge = inside.
        let d = (dx * (p[1] - a[1]) - dy * (p[0] - a[0])) / len;
        best = best.min(d);
    }
    best
}

/// Re-exported for tests: a Manifold from a part (world space).
pub fn solid(p: &Part) -> Result<Manifold, ModelerError> {
    crate::csg::part_to_manifold(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PrimitiveKind;

    fn model_of(parts: Vec<Part>) -> Model {
        Model {
            name: "m".into(),
            parts: parts.into_iter().map(Into::into).collect(),
            ..Default::default()
        }
    }

    fn cube(name: &str, size: f32, at: [f32; 3]) -> Part {
        let mut p = crate::primitives::primitive(PrimitiveKind::Box, [size; 3], name)
            .unwrap_or_else(|e| panic!("{e}"));
        p.translation = at;
        p
    }

    #[test]
    fn measure_reports_gap_touch_and_overlap() {
        let m = model_of(vec![
            cube("a", 1.0, [0.0, 0.5, 0.0]),
            cube("b", 1.0, [1.25, 0.5, 0.0]),
            cube("c", 1.0, [-1.0, 0.5, 0.0]),
            cube("d", 1.0, [0.0, 1.3, 0.0]),
        ]);
        let (_, j) = measure(&m, &["a".into(), "b".into()]).unwrap_or_else(|e| panic!("{e}"));
        let g = j["gap"].as_f64().unwrap_or(-1.0);
        assert!((g - 0.25).abs() < 1e-3, "{g}");
        let (t, _) = measure(&m, &["a".into(), "c".into()]).unwrap_or_else(|e| panic!("{e}"));
        assert!(t.contains("touching"), "{t}");
        let (t, j) = measure(&m, &["a".into(), "d".into()]).unwrap_or_else(|e| panic!("{e}"));
        assert!(t.contains("overlap"), "{t}");
        let ov = j["overlap_volume"].as_f64().unwrap_or(0.0);
        assert!((ov - 0.2).abs() < 1e-3, "{ov}");
        let (t, _) = measure(&m, &["d".into()]).unwrap_or_else(|e| panic!("{e}"));
        assert!(t.contains("floats"), "{t}");
    }

    #[test]
    fn section_through_a_hollow_box_finds_the_wall() {
        let outer = cube("o", 1.0, [0.0, 0.5, 0.0]);
        let inner = cube("i", 0.8, [0.0, 0.5, 0.0]);
        let mut scratch = model_of(vec![outer, inner]);
        let shell =
            crate::csg::boolean(&scratch, crate::BoolOp::Difference, "o", "i", Some("shell"))
                .unwrap_or_else(|e| panic!("{e}"));
        scratch.parts = vec![shell.into()];
        let (t, j, png) = section(&scratch, "y", 0.5, None).unwrap_or_else(|e| panic!("{e}"));
        let area = j["parts"][0]["area"].as_f64().unwrap_or(0.0);
        assert!((area - 0.36).abs() < 1e-3, "{t}");
        let wall = j["parts"][0]["thinnest_wall"].as_f64().unwrap_or(0.0);
        assert!((wall - 0.1).abs() < 1e-3, "{t}");
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
        let (t, _, _) = section(&scratch, "y", 5.0, None).unwrap_or_else(|e| panic!("{e}"));
        assert!(t.contains("misses"), "{t}");
    }

    #[test]
    fn mass_finds_the_centre_and_tipping() {
        // A post on a base: stable.
        let base = cube("base", 1.0, [0.0, 0.5, 0.0]);
        let mut post = cube("post", 0.2, [0.0, 1.1, 0.0]);
        post.scale = [1.0, 1.0, 1.0];
        let (t, j) = mass(
            &model_of(vec![base.clone(), post]),
            &BTreeMap::new(),
            1000.0,
        );
        assert!(j["stability"]["stable"].as_bool().unwrap_or(false), "{t}");
        let m = j["mass"].as_f64().unwrap_or(0.0);
        assert!((m - 1008.0).abs() < 0.1, "{m}");
        // A heavy overhang far off the base: tips.
        let arm = cube("arm", 1.0, [2.0, 1.5, 0.0]);
        let mut dens = BTreeMap::new();
        dens.insert("arm".to_string(), 5000.0);
        let (t, j) = mass(&model_of(vec![base, arm]), &dens, 1000.0);
        assert!(!j["stability"]["stable"].as_bool().unwrap_or(true), "{t}");
        assert!(t.contains("TIPS"), "{t}");
    }

    #[test]
    fn mass_of_one_object_uses_its_own_footprint() {
        let base = cube("lamp_base", 0.2, [5.0, 0.1, 0.0]);
        let mut arm = cube("lamp_arm", 0.2, [5.5, 0.3, 0.0]);
        arm.scale = [1.0, 1.0, 1.0];
        let big = cube("table", 2.0, [0.0, 1.0, 0.0]);
        let m = model_of(vec![base, arm, big]);
        let (t, j) = mass_of(&m, Some(&["lamp_*".to_string()]), &BTreeMap::new(), 1000.0);
        assert!(!j["stability"]["stable"].as_bool().unwrap_or(true), "{t}");
        assert!(!t.contains("`table`"), "{t}");
    }
}
