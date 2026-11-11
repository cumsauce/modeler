//! Deterministic verification over a [`Model`]: mesh validation, lints,
//! expectation checks and the rebuild diff. Plain functions over `&Model`, so
//! they test without a `Session`; the session methods just delegate here.

use crate::commands::CmdResult;
use crate::{Model, Op, Part, Vec3, graph, quat_of};

/// Signed volume of a part's mesh in world space (transform applied). Positive
/// for outward-wound closed meshes; reliable only as a sanity signal for
/// boolean outputs, which Manifold produces closed.
fn world_signed_volume(p: &Part) -> f64 {
    let rot = quat_of(p.rotation);
    let scale = Vec3::from(p.scale);
    let t = Vec3::from(p.translation);
    let mut vol = 0.0f64;
    for tri in p.indices.as_chunks::<3>().0 {
        let world = |i: u32| -> Vec3 { rot * (Vec3::from(p.verts[i as usize]) * scale) + t };
        let (a, b, c) = (world(tri[0]), world(tri[1]), world(tri[2]));
        vol += a.dot(b.cross(c)) as f64 / 6.0;
    }
    vol
}

/// Re-derives the whole graph into a scratch copy and compares the
/// geometry against the cached parts (names, verts, indices, transforms).
/// Catches a stale parts cache or a non-deterministic op. Baked edits
/// (set_material / visibility) are intentionally not compared: a refresh
/// legitimately resets those.
pub fn check_rebuild(model: &Model) -> CmdResult {
    let canon = |m: &Model| {
        m.parts
            .iter()
            .map(|p| {
                format!(
                    "{} {:?} {:?} {:?} {:?} {:?}",
                    p.name, p.verts, p.indices, p.translation, p.rotation, p.scale
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mut clone = model.clone();
    clone.refresh_uncached().map_err(|e| e.to_string())?;
    if canon(model) == canon(&clone) {
        Ok(format!(
            "cached parts match a fresh re-derivation ({} parts)",
            model.parts.len()
        ))
    } else {
        Err("cached parts differ from a fresh re-derivation — the parts cache is stale or an op is non-deterministic".into())
    }
}

/// Deterministic part-list assertion: compares the model's actual part
/// names against `expected` exactly (order-insensitive). Catches missing
/// array copies, duplicated radial sources, and stray boolean
/// intermediates — the "7 cylinders where I designed 6" class of bug —
/// without a screenshot.
pub fn expect_parts(model: &Model, expected: &[String]) -> CmdResult {
    // Hidden parts are not counted: they do not render or export, so an
    // assertion about "what the model is" should not trip over them.
    let actual: std::collections::BTreeSet<&str> = model
        .parts
        .iter()
        .filter(|p| p.visible)
        .map(|p| p.name.as_str())
        .collect();
    let hidden: Vec<&str> = model
        .parts
        .iter()
        .filter(|p| !p.visible)
        .map(|p| p.name.as_str())
        .collect();
    // `prefix*` entries match (and cover) every part with that prefix.
    let (globs, exact): (Vec<&str>, Vec<&str>) = expected
        .iter()
        .map(String::as_str)
        .partition(|e| e.ends_with('*'));
    let prefixes: Vec<&str> = globs.iter().map(|g| g.trim_end_matches('*')).collect();
    let expected: std::collections::BTreeSet<&str> = exact.into_iter().collect();
    let mut missing: Vec<&str> = expected.difference(&actual).copied().collect();
    missing.extend(
        globs
            .iter()
            .zip(&prefixes)
            .filter(|(_, p)| !actual.iter().any(|a| a.starts_with(**p)))
            .map(|(g, _)| *g),
    );
    let extra: Vec<_> = actual
        .difference(&expected)
        .filter(|a| !prefixes.iter().any(|p| a.starts_with(p)))
        .collect();
    if missing.is_empty() && extra.is_empty() {
        let suffix = if hidden.is_empty() {
            String::new()
        } else {
            format!(" ({} hidden parts not counted)", hidden.len())
        };
        Ok(format!(
            "all {} expected parts present, no extras{suffix}",
            expected.len()
        ))
    } else {
        let mut msg = Vec::new();
        if !missing.is_empty() {
            msg.push(format!("missing: {missing:?}"));
        }
        if !extra.is_empty() {
            msg.push(format!("unexpected: {extra:?}"));
        }
        Err(msg.join("; ").into())
    }
}

/// Numeric bounds assertion over the visible parts: the model's world
/// bounds must sit within `tol` of `min`/`max` per axis (compare only the
/// axes you care about by passing `None`). This is the deterministic form
/// of the eyeball checks — "it sits on the ground" is `min` y ≈ 0, "it is
/// centered" is symmetric min/max x/z, "nothing floats" is a max y near
/// the design height. Hidden parts are not counted (they are not
/// rendered or exported).
pub fn expect_bounds(
    model: &Model,
    min: [Option<f32>; 3],
    max: [Option<f32>; 3],
    tol: f32,
) -> CmdResult {
    let Some((amin, amax)) = model.bounds() else {
        return Err("no visible parts to bound".into());
    };
    let mut issues = Vec::new();
    for (axis, name) in [(0, "x"), (1, "y"), (2, "z")] {
        for (want, got, lo) in [
            (min[axis], amin[axis], true),
            (max[axis], amax[axis], false),
        ] {
            let Some(want) = want else { continue };
            if (want - got).abs() > tol {
                // Name the visible part that sets this extreme.
                let setter = model
                    .parts
                    .iter()
                    .filter(|p| p.visible && !p.verts.is_empty())
                    .map(|p| {
                        let (pmin, pmax) = p.bounds();
                        (p.name.as_str(), if lo { pmin[axis] } else { pmax[axis] })
                    })
                    .min_by(|a, b| {
                        let (x, y) = if lo { (a.1, b.1) } else { (b.1, a.1) };
                        x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .map(|(n, _)| format!(", set by `{n}`"))
                    .unwrap_or_default();
                issues.push(format!(
                    "{} {} (expected {want:.4} ±{tol}, got {got:.4}{setter})",
                    if lo { "min" } else { "max" },
                    name
                ));
            }
        }
    }
    if issues.is_empty() {
        Ok(format!("bounds within ±{tol}: min {amin:?}, max {amax:?}"))
    } else {
        Err(format!("bounds off: {}", issues.join("; ")).into())
    }
}

/// Weld grid for a part: 1e-6 of its size, clamped to [1e-9, 1e-5], so
/// real-scale centimetre parts keep their features apart.
fn weld_grid(p: &Part) -> f64 {
    let (lo, hi) = p.bounds();
    let d = (hi - lo).length() as f64;
    (d * 1e-6).clamp(1e-9, 1e-5)
}

/// Edges of a part that are not shared by exactly two triangles, after
/// welding coincident vertices. Two welds are tried and the smaller count
/// wins: a 1e-5 grid (primitives emit per-face vertices whose positions differ
/// in the last bits) and exact positions (Manifold outputs keep distinct
/// vertices closer than any grid). A genuinely open mesh is open under both.
pub fn open_edge_count(p: &Part) -> usize {
    let inv = 1.0 / weld_grid(p);
    let grid = open_edges_with(p, |v| {
        [
            (v[0] as f64 * inv).round() as i64,
            (v[1] as f64 * inv).round() as i64,
            (v[2] as f64 * inv).round() as i64,
        ]
    });
    if grid == 0 {
        return 0;
    }
    // +0.0 so -0.0 and 0.0 weld together.
    let exact = open_edges_with(p, |v| {
        [
            (v[0] + 0.0).to_bits() as i64,
            (v[1] + 0.0).to_bits() as i64,
            (v[2] + 0.0).to_bits() as i64,
        ]
    });
    grid.min(exact)
}

fn open_edges_with(p: &Part, key: impl Fn(&[f32; 3]) -> [i64; 3]) -> usize {
    let mut ids: std::collections::HashMap<[i64; 3], u32> = std::collections::HashMap::new();
    let welded: Vec<u32> = p
        .verts
        .iter()
        .map(|v| {
            let next = ids.len() as u32;
            *ids.entry(key(v)).or_insert(next)
        })
        .collect();
    // Net directed count per undirected edge: +1 a->b, -1 b->a. A closed,
    // consistently wound surface uses every edge once each way (net 0). Two
    // surfaces touching along a line (Manifold keeps them as separate
    // vertices at one position) give 4 uses, still net 0; an open edge or a
    // flipped neighbour leaves a non-zero net.
    let mut edges: std::collections::HashMap<(u32, u32), i32> = std::collections::HashMap::new();
    for t in p.indices.as_chunks::<3>().0 {
        for e in 0..3 {
            let (a, b) = (welded[t[e] as usize], welded[t[(e + 1) % 3] as usize]);
            if a == b {
                continue;
            }
            let (k, s) = if a < b { ((a, b), 1) } else { ((b, a), -1) };
            *edges.entry(k).or_insert(0) += s;
        }
    }
    edges.values().filter(|c| **c != 0).count()
}

/// Area enclosed by a part's unmatched (boundary) half-edges, as a fraction of
/// its surface area. Hairline seams and T-junction cracks bound ~zero area, while
/// a missing wall bounds the wall's whole area, so this separates harmless cut
/// seams from see-through holes. Returns (enclosed area, surface area).
pub fn boundary_gap_area(p: &Part) -> (f64, f64) {
    use std::collections::HashMap;
    let mut ids: HashMap<[i64; 3], u32> = HashMap::new();
    let mut welded = Vec::with_capacity(p.verts.len());
    let inv = 1.0 / weld_grid(p);
    for v in &p.verts {
        let key = [
            (v[0] as f64 * inv).round() as i64,
            (v[1] as f64 * inv).round() as i64,
            (v[2] as f64 * inv).round() as i64,
        ];
        let next = ids.len() as u32;
        welded.push(*ids.entry(key).or_insert(next));
    }
    let pos = |i: u32| {
        let v = p.verts[i as usize];
        [f64::from(v[0]), f64::from(v[1]), f64::from(v[2])]
    };
    let mut half: HashMap<(u32, u32), (i32, u32)> = HashMap::new();
    let mut surface = 0.0f64;
    for t in p.indices.as_chunks::<3>().0 {
        let (a, b, c) = (pos(t[0]), pos(t[1]), pos(t[2]));
        let (u, w) = (
            [b[0] - a[0], b[1] - a[1], b[2] - a[2]],
            [c[0] - a[0], c[1] - a[1], c[2] - a[2]],
        );
        let cr = [
            u[1] * w[2] - u[2] * w[1],
            u[2] * w[0] - u[0] * w[2],
            u[0] * w[1] - u[1] * w[0],
        ];
        surface += 0.5 * (cr[0] * cr[0] + cr[1] * cr[1] + cr[2] * cr[2]).sqrt();
        for e in 0..3 {
            let (i, j) = (t[e], t[(e + 1) % 3]);
            let (wi, wj) = (welded[i as usize], welded[j as usize]);
            if wi == wj {
                continue;
            }
            // Net count per undirected edge: +1 forward, -1 backward.
            let (key, sign) = if wi < wj {
                ((wi, wj), 1)
            } else {
                ((wj, wi), -1)
            };
            let entry = half.entry(key).or_insert((0, i));
            entry.0 += sign;
            if sign == 1 {
                entry.1 = i;
            }
        }
    }
    let mut first_vert = vec![0u32; ids.len()];
    for (i, w) in welded.iter().enumerate().rev() {
        first_vert[*w as usize] = i as u32;
    }
    let rep = |w: u32| first_vert[w as usize];
    // Remaining net edges are unmatched; sum their vector area (1/2 sum a x b).
    let mut acc = [0.0f64; 3];
    for (&(lo, hi), &(net, _)) in &half {
        if net == 0 {
            continue;
        }
        let (a, b) = if net > 0 {
            (pos(rep(lo)), pos(rep(hi)))
        } else {
            (pos(rep(hi)), pos(rep(lo)))
        };
        acc[0] += a[1] * b[2] - a[2] * b[1];
        acc[1] += a[2] * b[0] - a[0] * b[2];
        acc[2] += a[0] * b[1] - a[1] * b[0];
    }
    let enclosed = 0.5 * (acc[0] * acc[0] + acc[1] * acc[1] + acc[2] * acc[2]).sqrt();
    (enclosed, surface)
}

/// The name a part ends up with after any `Rename` ops that follow it.
fn final_name_after_renames(ops: &[Op], name: &str) -> String {
    let mut cur = name.to_string();
    for op in ops {
        if let Op::Rename { name: to, input } = op
            && *input == cur
        {
            cur = to.clone();
        }
    }
    cur
}

/// Mesh-level checks per part: edge/face manifoldness and winding (signed
/// volume must be positive for an outward-wound closed mesh). Pure mesh
/// math — no renderer needed — so it catches inverted or torn geometry
/// that a screenshot only suggests. Edges are welded by quantized vertex
/// position, since primitives emit per-face vertices that share a point.
pub fn validate(model: &Model) -> String {
    let mut lines = Vec::new();
    if model.parts.is_empty() {
        return "no parts to validate".into();
    }
    for p in &model.parts {
        if p.verts.is_empty() {
            continue;
        }
        let open = open_edge_count(p);
        if open > 0 {
            lines.push(format!(
                "- `{}`: not closed ({open} open or inconsistently wound edges)",
                p.name
            ));
        }
        // Signed volume: positive for outward-wound closed meshes.
        let mut vol = 0.0f64;
        for t in p.indices.as_chunks::<3>().0 {
            let (a, b, c) = (
                Vec3::from(p.verts[t[0] as usize]),
                Vec3::from(p.verts[t[1] as usize]),
                Vec3::from(p.verts[t[2] as usize]),
            );
            vol += a.dot(b.cross(c)) as f64 / 6.0;
        }
        if open == 0 && vol < 0.0 {
            lines.push(format!(
                "- `{}`: inverted winding (signed volume {vol:.3} < 0)",
                p.name
            ));
        }
    }
    if lines.is_empty() {
        "all parts: manifold, outward-wound".into()
    } else {
        lines.join("\n")
    }
}

/// A volume in m³ with enough significant digits at any scale.
fn fmt_vol(v: f64) -> String {
    if v.abs() >= 0.01 {
        format!("{v:.4} m³")
    } else {
        format!("{v:.3e} m³")
    }
}

/// True when `part` only gathers cutters: it (or a union it feeds) is the
/// `b` of a difference. Disjoint cutters are unioned on purpose.
fn is_cutter(ops: &[Op], part: &str) -> bool {
    ops.iter().any(|op| match op {
        Op::Boolean {
            op: crate::BoolOp::Difference,
            a,
            b,
            ..
        } => b == part && a != part,
        Op::Boolean {
            op: crate::BoolOp::Union,
            a,
            b,
            ..
        } if a == part || b == part => {
            let out = graph::output(op);
            out != part && is_cutter(ops, out)
        }
        Op::Group { name, inputs } if inputs.iter().any(|i| i == part) => {
            name != part && is_cutter(ops, name)
        }
        _ => false,
    })
}

/// Cheap deterministic warnings about the current graph/parts, appended to
/// `scene_summary`. These encode the ordering and overlap traps that are
/// easy to hit and expensive to spot in a render.
pub fn lints(model: &Model) -> Vec<String> {
    let mut out = Vec::new();
    // Boolean volume sanity: a difference must shrink its input, a union
    // must not fall below its larger input, and a union that exactly sums
    // its inputs had no overlap at all.
    for op in &model.ops {
        let Op::Boolean {
            name,
            op: kind,
            a,
            b,
        } = op
        else {
            continue;
        };
        let part = |n: &str| model.parts.iter().find(|p| p.name == n);
        let out_name = graph::output(op);
        let (Some(pa), Some(pb), Some(po)) = (part(a), part(b), part(out_name)) else {
            continue;
        };
        let (va, vb, vo) = (
            world_signed_volume(pa),
            world_signed_volume(pb),
            world_signed_volume(po),
        );
        let tol = (va.abs().max(vb.abs()).max(vo.abs()) * 1e-3).max(1e-15);
        // "No overlap" must see a small part sunk a few mm into a big one
        // (a handle into a pot): scale with the smaller input, above f32 noise.
        let sum_tol = (va.abs().min(vb.abs()) * 1e-4)
            .max(va.abs().max(vb.abs()) * 1e-6)
            .max(1e-15);
        match kind.as_str() {
            "difference" => {
                if vo > va - tol {
                    out.push(format!(
                        "difference `{name}` left `{a}` at its original volume ({} vs {}) — the cutter `{b}` probably misses it",
                        fmt_vol(vo),
                        fmt_vol(va)
                    ));
                }
            }
            "union" => {
                if vo < va.abs().max(vb.abs()) - tol {
                    out.push(format!(
                        "union `{name}` is smaller than its larger input ({}) — inverted or torn result",
                        fmt_vol(vo)
                    ));
                } else if (vo - (va + vb)).abs() <= sum_tol && !is_cutter(&model.ops, out_name) {
                    out.push(format!(
                        "union `{name}` volume is exactly `{a}` + `{b}` ({}) — the inputs have no overlap",
                        fmt_vol(vo)
                    ));
                }
            }
            _ => {}
        }
    }
    // Boolean outputs with open edges that enclose real area. Manifold outputs
    // are closed, so this is a safety net: curved cuts can show a few
    // weld-grid open edges that enclose ~0 area and stay silent.
    for op in &model.ops {
        if let Op::Boolean { name, .. } = op
            && let final_name = final_name_after_renames(&model.ops, name)
            && let Some(p) = model
                .parts
                .iter()
                .find(|p| p.name == final_name && p.visible)
        {
            let open = open_edge_count(p);
            let (gap, surface) = boundary_gap_area(p);
            if open > 0 && gap > 0.002 * surface {
                out.push(format!(
                    "boolean `{}` has {open} open edges enclosing {gap:.3} m² of missing surface ({:.1}% of its area) — likely a hole; check with probe_ray through any pocket/opening",
                    p.name,
                    100.0 * gap / surface.max(1e-9)
                ));
            }
        }
    }
    // Full-ring radial array with the source on the first copy's angle.
    // Skipped when the source part is hidden: an invisible overlap is
    // harmless (nothing renders there).
    let part_by_name = |n: &str| model.parts.iter().find(|p| p.name == n);
    for op in &model.ops {
        if let Op::RadialArray {
            name,
            input,
            count,
            degrees,
            start_degrees,
            include_source,
            ..
        } = op
            && *count > 1
            && !*include_source
            && !part_by_name(input).is_some_and(|p| !p.visible)
            && degrees.rem_euclid(360.0) == 0.0
            && start_degrees.rem_euclid(360.0) == 0.0
        {
            out.push(format!(
                "radial `{name}`: full ring with the source at the first copy's angle — the source and copy 0 overlap. Use count-1 copies with start_degrees = degrees/count."
            ));
        }
    }
    // Parts still wearing the default material. Hidden parts are skipped:
    // they are not rendered, so their color is irrelevant noise.
    for p in &model.parts {
        if !p.visible {
            continue;
        }
        let m = &p.material;
        if m.base_color == [0.7, 0.7, 0.7, 1.0] && m.roughness == 0.6 && m.metalness == 0.0 {
            out.push(format!(
                "part `{}` still has the default material (forgot an op_material?)",
                p.name
            ));
        }
    }
    // Coincident world bounds → z-fighting.
    for (i, a) in model.parts.iter().enumerate() {
        if !a.visible {
            continue;
        }
        for b in &model.parts[i + 1..] {
            if !b.visible {
                continue;
            }
            let (amin, amax) = a.bounds();
            let (bmin, bmax) = b.bounds();
            let near = |x: f32, y: f32| (x - y).abs() < 1e-4;
            if near(amin.x, bmin.x)
                && near(amin.y, bmin.y)
                && near(amin.z, bmin.z)
                && near(amax.x, bmax.x)
                && near(amax.y, bmax.y)
                && near(amax.z, bmax.z)
            {
                out.push(format!(
                    "parts `{}` and `{}` occupy identical bounds — their faces will z-fight",
                    a.name, b.name
                ));
            }
        }
    }
    out.extend(floating_parts(model).into_iter().map(|n| {
        format!("part `{n}` floats: it is off the ground and touches no other visible part")
    }));
    out
}

/// Visible parts that are off the ground (min y above 0) and whose world
/// bounds touch no other visible part: a copy moved instead of its
/// neighbour, a cap that missed its column. Bounds are conservative (a
/// touching pair may be reported as touching while not quite), so a part
/// named here truly hangs in the air.
pub fn floating_parts(model: &Model) -> Vec<String> {
    let vis: Vec<(&str, Vec3, Vec3)> = model
        .parts
        .iter()
        .filter(|p| p.visible && !p.verts.is_empty())
        .map(|p| {
            let (lo, hi) = p.bounds();
            (p.name.as_str(), lo, hi)
        })
        .collect();
    if vis.len() < 2 {
        return Vec::new();
    }
    let (mlo, mhi) = vis
        .iter()
        .fold((Vec3::MAX, Vec3::MIN), |(a, b), (_, lo, hi)| {
            (a.min(*lo), b.max(*hi))
        });
    let tol = ((mhi - mlo).length() * 1e-4).max(1e-5);
    let ground = mlo.y.min(0.0);
    vis.iter()
        .filter(|(_, lo, _)| lo.y > ground + tol)
        .filter(|(name, lo, hi)| {
            !vis.iter().any(|(other, olo, ohi)| {
                other != name
                    && lo.x <= ohi.x + tol
                    && olo.x <= hi.x + tol
                    && lo.y <= ohi.y + tol
                    && olo.y <= hi.y + tol
                    && lo.z <= ohi.z + tol
                    && olo.z <= hi.z + tol
            })
        })
        .map(|(n, _, _)| n.to_string())
        .collect()
}

/// Non-strict part-list assertion: every name in `wanted` is a visible part
/// (extras allowed). A trailing `*` matches a prefix (`gz.*`).
pub fn expect_includes(model: &Model, wanted: &[String]) -> CmdResult {
    let vis: Vec<&str> = model
        .parts
        .iter()
        .filter(|p| p.visible)
        .map(|p| p.name.as_str())
        .collect();
    let missing: Vec<&String> = wanted
        .iter()
        .filter(|w| !vis.iter().any(|v| crate::name_matches(w, v)))
        .collect();
    if missing.is_empty() {
        Ok(format!(
            "all {} wanted parts present ({} visible)",
            wanted.len(),
            vis.len()
        ))
    } else {
        Err(format!("missing: {missing:?}").into())
    }
}

/// Every visible part touches another or the ground (see [`floating_parts`]).
pub fn expect_grounded(model: &Model) -> CmdResult {
    let f = floating_parts(model);
    if f.is_empty() {
        Ok("every visible part touches the ground or another part".into())
    } else {
        Err(format!("floating: {}", f.join(", ")).into())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn gap_area_sees_missing_wall_but_not_closed_mesh() {
        let Ok(mut b) = crate::primitives::primitive(crate::PrimitiveKind::Box, [1.0; 3], "b")
        else {
            panic!("box failed");
        };
        let (gap, surf) = super::boundary_gap_area(&b);
        assert!(gap < 1e-9, "closed box encloses no gap, got {gap}");
        assert!((surf - 6.0).abs() < 1e-4);
        // Drop one triangle pair worth of faces: remove the last two triangles.
        let n = b.indices.len();
        b.indices.truncate(n - 6);
        let (gap, _) = super::boundary_gap_area(&b);
        assert!((gap - 1.0).abs() < 1e-3, "missing 1 m² face, got {gap}");
    }

    use super::*;

    #[test]
    fn verify_runs_on_a_bare_model_without_a_session() {
        let mut model = Model::named("bare");
        let part = crate::primitives::primitive(crate::PrimitiveKind::Box, [1.0; 3], "b")
            .unwrap_or_else(|e| panic!("{e}"));
        model.parts.push(part.into());
        assert!(expect_parts(&model, &["b".to_string()]).is_ok());
        assert!(expect_parts(&model, &["nope".to_string()]).is_err());
        assert!(validate(&model).contains("manifold, outward-wound"));
        assert!(lints(&model).iter().all(|l| !l.is_empty()));
    }
}
