//! Boolean operations on parts via Manifold (`manifold-csg`, bindings to the C++
//! kernel OpenSCAD and Blender use). Both parts are transformed into model space
//! (translation/rotation/scale baked in) and carry their UVs as vertex properties;
//! the result is a closed, manifold triangle `Part` with interpolated UVs, creased
//! normals and part `a`'s material. Coplanar faces are resolved exactly by the
//! kernel, so no nudging or welding is needed.

use crate::{BoolOp, Model, ModelerError, Part, quat_of};
use bevy::math::{Quat, Vec3};
use manifold_csg::{Manifold, MeshGL64};

/// Vertex properties per vertex: position (3) + UV (2).
const N_PROPS: usize = 5;

fn run_op(op: BoolOp, a: &Manifold, b: &Manifold) -> Manifold {
    match op {
        BoolOp::Union => a.union(b),
        BoolOp::Difference => a.difference(b),
        BoolOp::Intersect => a.intersection(b),
    }
}

/// A part as a Manifold: transform baked into the positions, UVs carried as
/// properties. Vertices split only for UV/normal seams are re-joined by
/// `MeshGL64::merge` (by position), which is what makes a primitive's
/// seam-split mesh a closed solid to Manifold. Errors when the part is not a
/// closed solid (open sheets like disc/plane/washer, torn meshes).
pub(crate) fn part_to_manifold(part: &Part) -> Result<Manifold, ModelerError> {
    let rot = quat_of(part.rotation);
    let scale = Vec3::from(part.scale);
    let t = Vec3::from(part.translation);
    // Parts without UVs get a box projection first so every vertex has one.
    let projected;
    let src = if part.uvs.len() == part.verts.len() {
        part
    } else {
        let mut p = part.clone();
        let _ = crate::uv::ensure_uvs(&mut p, Some("box"));
        projected = p;
        &projected
    };
    // In f64: a small part far from the origin (a scattered rock at x = 10)
    // would otherwise round its vertices to f32 steps coarser than the weld
    // grid below, splitting poles and seams.
    let (rot, scale, t) = (rot.as_dquat(), scale.as_dvec3(), t.as_dvec3());
    let mut props = Vec::with_capacity(src.verts.len() * N_PROPS);
    for (i, v) in src.verts.iter().enumerate() {
        let p = rot * (Vec3::from(*v).as_dvec3() * scale) + t;
        let uv = src.uvs.get(i).copied().unwrap_or([0.0; 2]);
        props.extend([p.x, p.y, p.z, uv[0] as f64, uv[1] as f64]);
    }
    let count = src.verts.len();
    let tris: Vec<usize> = if src.indices.is_empty() {
        (0..count - count % 3).collect()
    } else {
        src.indices.iter().map(|&i| i as usize).collect()
    };
    if tris.iter().any(|&i| i >= count) {
        return Err(ModelerError::Csg(format!(
            "`{}` has out-of-range triangle indices",
            part.name
        )));
    }
    // Weld by position (1e-5 grid): verts split only for UV/normal seams become
    // merge pairs, so Manifold sees one topological vertex but keeps both UVs.
    // The grid scales with the part (1e-6 of its size, so 1e-5 at 10 m and
    // 1e-7 at 10 cm): seam duplicates differ only in the last f32 bits, while
    // real features of small (real-scale) parts stay apart.
    let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
    for v in props.chunks(N_PROPS) {
        for k in 0..3 {
            lo[k] = lo[k].min(v[k]);
            hi[k] = hi[k].max(v[k]);
        }
    }
    let diag = (0..3).map(|k| (hi[k] - lo[k]).powi(2)).sum::<f64>().sqrt();
    let inv = 1.0 / (diag * 1e-6).clamp(1e-9, 1e-5);
    let key = |i: usize| -> [i64; 3] {
        [
            (props[i * N_PROPS] * inv).round() as i64,
            (props[i * N_PROPS + 1] * inv).round() as i64,
            (props[i * N_PROPS + 2] * inv).round() as i64,
        ]
    };
    // Two copies of one vertex can straddle a cell border (a seam vertex at
    // x = 0.08 off by one ulp), so look in the neighbouring cells too and
    // weld anything within one cell size.
    let tol2 = (1.0 / inv).powi(2);
    let mut cells: std::collections::HashMap<[i64; 3], Vec<usize>> = Default::default();
    let mut rep = vec![0usize; count];
    let (mut merge_from, mut merge_to) = (Vec::new(), Vec::new());
    for (i, r) in rep.iter_mut().enumerate() {
        let k = key(i);
        let p = &props[i * N_PROPS..i * N_PROPS + 3];
        let mut found = None;
        'search: for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    let Some(list) = cells.get(&[k[0] + dx, k[1] + dy, k[2] + dz]) else {
                        continue;
                    };
                    for &j in list {
                        let q = &props[j * N_PROPS..j * N_PROPS + 3];
                        let d2 =
                            (p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2);
                        if d2 <= tol2 {
                            found = Some(j);
                            break 'search;
                        }
                    }
                }
            }
        }
        let f = match found {
            Some(j) => j,
            None => {
                cells.entry(k).or_default().push(i);
                i
            }
        };
        *r = f;
        if f != i {
            merge_from.push(i as u64);
            merge_to.push(f as u64);
        }
    }
    // Drop triangles that collapse once welded (e.g. UV-sphere pole fans).
    let mut kept: Vec<u64> = Vec::with_capacity(tris.len());
    for t in tris.as_chunks::<3>().0 {
        let r = [rep[t[0]], rep[t[1]], rep[t[2]]];
        if r[0] != r[1] && r[1] != r[2] && r[0] != r[2] {
            kept.extend(t.iter().map(|&i| i as u64));
        }
    }
    let not_solid = |e: String| {
        let open = crate::verify::open_edge_count(part);
        let why = if open > 0 {
            format!("{open} open edges: it has holes or is a sheet")
        } else {
            "its surface is closed but inside-out or self-intersecting".to_string()
        };
        ModelerError::Csg(format!(
            "`{}` is not a closed solid ({e}; {why}). Booleans, round and inspect need closed meshes: \
             disc/plane/washer are open sheets (use a thin box, cylinder or extrude instead), \
             and tri_mesh parts must be watertight",
            part.name
        ))
    };
    let opts = manifold_csg::MeshGL64Options::new().merge_vertices(&merge_from, &merge_to);
    let mesh = MeshGL64::new_with_options(&props, N_PROPS, &kept, opts)
        .map_err(|e| not_solid(e.to_string()))?;
    let m = Manifold::from_meshgl64(&mesh).map_err(|e| not_solid(e.to_string()))?;
    m.status().map_err(|e| not_solid(e.to_string()))?;
    // A double-sided sheet is "closed" topologically but encloses nothing.
    if m.volume().abs() < 1e-9 {
        return Err(not_solid("zero volume".into()));
    }
    Ok(m)
}

/// A Manifold back into a triangle `Part`: positions and interpolated UVs from
/// the properties, creased normals (40°, so cut edges stay hard), `material`.
pub(crate) fn manifold_to_part(m: &Manifold, name: &str, material: crate::Material) -> Part {
    manifold_to_part_mapped(m, name, material, |v| v)
}

/// [`manifold_to_part`] with every position passed through `map` first.
fn manifold_to_part_mapped(
    m: &Manifold,
    name: &str,
    material: crate::Material,
    map: impl Fn([f32; 3]) -> [f32; 3],
) -> Part {
    let mesh = m.to_meshgl64();
    let n = mesh.num_prop().max(3);
    let props = mesh.vert_properties();
    let mut part = Part::new(name);
    part.material = material;
    for v in props.chunks(n) {
        part.verts
            .push(map([v[0] as f32, v[1] as f32, v[2] as f32]));
        if n >= N_PROPS {
            part.uvs.push([v[3] as f32, v[4] as f32]);
        }
    }
    part.indices = mesh.tri_verts().iter().map(|&i| i as u32).collect();
    if part.uvs.len() != part.verts.len() {
        part.uvs.clear();
    }
    crate::uv::crease_normals(&mut part, 40.0);
    if part.uvs.len() != part.verts.len() {
        let _ = crate::uv::ensure_uvs(&mut part, Some("box"));
    }
    part
}

/// Boolean `op` on the named parts of `model`, returning a new part named
/// `name.unwrap_or("{a}-{op}-{b}")` with `a`'s material. Errors on unknown parts,
/// non-solid inputs and empty results (no overlap, ...).
pub fn boolean(
    model: &Model,
    op: BoolOp,
    a: &str,
    b: &str,
    name: Option<&str>,
) -> Result<Part, ModelerError> {
    let pa = model
        .part(a)
        .ok_or_else(|| ModelerError::NoPart(a.to_string()))?;
    let pb = model
        .part(b)
        .ok_or_else(|| ModelerError::NoPart(b.to_string()))?;
    let result = run_op(op, &part_to_manifold(pa)?, &part_to_manifold(pb)?);
    result
        .status()
        .map_err(|e| ModelerError::Csg(format!("{op}({a}, {b}) failed: {e}")))?;
    if result.is_empty() {
        return Err(ModelerError::Csg(explain_empty(op, a, pa, b, pb)));
    }
    let default_name = format!("{a}-{op}-{b}");
    let mut part = manifold_to_part(&result, name.unwrap_or(&default_name), pa.material.clone());
    // The transform is baked into the verts, so the part sits at the model root.
    part.translation = [0.0; 3];
    part.rotation = Quat::IDENTITY.to_array();
    part.scale = [1.0; 3];
    Ok(part)
}

/// Why a boolean came out empty, in terms of the inputs' world boxes: which
/// axis separates them and by how much, or that the cutter swallows `a`.
fn explain_empty(op: BoolOp, a: &str, pa: &Part, b: &str, pb: &Part) -> String {
    let (alo, ahi) = pa.bounds();
    let (blo, bhi) = pb.bounds();
    let fmt = |lo: Vec3, hi: Vec3| {
        format!(
            "[{:.3}, {:.3}, {:.3}]..[{:.3}, {:.3}, {:.3}]",
            lo.x, lo.y, lo.z, hi.x, hi.y, hi.z
        )
    };
    let boxes = format!(
        "`{a}` spans {}, `{b}` spans {}",
        fmt(alo, ahi),
        fmt(blo, bhi)
    );
    let gaps: Vec<(usize, f32)> = (0..3)
        .map(|k| (k, (blo[k] - ahi[k]).max(alo[k] - bhi[k])))
        .filter(|(_, g)| *g > 0.0)
        .collect();
    let axis = |k: usize| ["x", "y", "z"][k];
    match op {
        BoolOp::Difference => format!(
            "difference({a}, {b}) is empty: `{b}` covers all of `{a}`, so nothing is left. Did you swap a and b (a is kept, b is cut away)? {boxes}"
        ),
        _ if !gaps.is_empty() => format!(
            "{op}({a}, {b}) is empty: the parts don't touch, they are {} apart. Move one, or use group to combine separate parts. {boxes}",
            gaps.iter()
                .map(|(k, g)| format!("{g:.4} m along {}", axis(*k)))
                .collect::<Vec<_>>()
                .join(" and ")
        ),
        _ => format!(
            "{op}({a}, {b}) is empty: the boxes overlap but the solids don't (e.g. one sits in the other's hollow or a ring's hole). {boxes}"
        ),
    }
}

/// Manifold builds sweeps Z-up; the modeler is Y-up. (x, y, z) -> (x, z, -y)
/// is a proper rotation (-90° about X), so winding stays outward.
fn z_up_to_y_up(v: [f32; 3]) -> [f32; 3] {
    [v[0], v[2], -v[1]]
}

fn check_profile(profile: &[Vec<[f32; 2]>]) -> Result<(), ModelerError> {
    if profile.is_empty() {
        return Err(ModelerError::Csg("profile has no rings".into()));
    }
    for (i, ring) in profile.iter().enumerate() {
        if ring.len() < 3 {
            return Err(ModelerError::Csg(format!(
                "profile ring {i} has {} points; a ring needs at least 3",
                ring.len()
            )));
        }
        if ring.iter().flatten().any(|c| !c.is_finite()) {
            return Err(ModelerError::Csg(format!(
                "profile ring {i} has a non-finite coordinate"
            )));
        }
    }
    Ok(())
}

fn cross_section(rings: Vec<Vec<[f64; 2]>>) -> manifold_csg::CrossSection {
    // Even-odd: ring orientation doesn't matter, and an inner ring is a hole.
    manifold_csg::CrossSection::from_polygons_with_fill_rule(
        &rings,
        manifold_csg::FillRule::EvenOdd,
    )
}

/// Extrudes font outlines straight up (as `extrude`, no twist or taper)
/// with the non-zero fill rule fonts are drawn with, so glyphs that overlap
/// (tight letter spacing, script joins, overlapping contours) merge instead
/// of cutting holes in each other.
pub fn extrude_glyphs(
    name: &str,
    profile: &[Vec<[f32; 2]>],
    height: f32,
) -> Result<Part, ModelerError> {
    check_profile(profile)?;
    if height.is_nan() || height <= 0.0 {
        return Err(ModelerError::Csg(format!(
            "text `{name}`: depth must be > 0, got {height}"
        )));
    }
    let rings: Vec<Vec<[f64; 2]>> = profile
        .iter()
        .map(|r| r.iter().map(|p| [p[0] as f64, -p[1] as f64]).collect())
        .collect();
    let cs = manifold_csg::CrossSection::from_polygons_with_fill_rule(
        &rings,
        manifold_csg::FillRule::NonZero,
    );
    let m = Manifold::extrude_with_options(&cs, height as f64, 1, 0.0, 1.0, 1.0);
    finish_sweep(&m, name, "text")
}

fn finish_sweep(m: &Manifold, name: &str, what: &str) -> Result<Part, ModelerError> {
    m.status()
        .map_err(|e| ModelerError::Csg(format!("{what} `{name}` failed: {e}")))?;
    if m.is_empty() || m.volume().abs() < 1e-12 {
        return Err(ModelerError::Csg(format!(
            "{what} `{name}` is empty (degenerate or self-cancelling profile)"
        )));
    }
    Ok(manifold_to_part_mapped(
        m,
        name,
        crate::Material::default(),
        z_up_to_y_up,
    ))
}

/// Extrudes a 2D footprint straight up. `profile` rings are [x, z] points on
/// the ground plane (inner rings are holes); the solid spans y = 0..height,
/// optionally twisting (degrees) and scaling the top ring ([sx, sz]); twist
/// and scale act about the profile origin [0, 0], so centre the footprint on
/// it for a symmetric taper.
pub fn extrude(
    name: &str,
    profile: &[Vec<[f32; 2]>],
    height: f32,
    twist: f32,
    scale_top: [f32; 2],
    slices: u32,
    round: f32,
) -> Result<Part, ModelerError> {
    check_profile(profile)?;
    if height.is_nan() || height <= 0.0 {
        return Err(ModelerError::Csg(format!(
            "extrude `{name}`: height must be > 0, got {height}"
        )));
    }
    if scale_top.iter().any(|s| s.is_nan() || *s < 0.0) {
        return Err(ModelerError::Csg(format!(
            "extrude `{name}`: scale_top must be >= 0, got {scale_top:?}"
        )));
    }
    // Footprint (x, z) -> Manifold (x, -z) so it lands at world z after the
    // Z-up -> Y-up rotation.
    let rings = profile
        .iter()
        .map(|r| r.iter().map(|p| [p[0] as f64, -p[1] as f64]).collect())
        .collect();
    let cs = round_cross_section(cross_section(rings), round);
    // Twist runs the other way after the axis swap; negate so positive
    // twist is counter-clockwise seen from above, like a y-axis rotation.
    let m = Manifold::extrude_with_options(
        &cs,
        height as f64,
        slices as i32,
        -(twist as f64),
        scale_top[0] as f64,
        scale_top[1] as f64,
    );
    finish_sweep(&m, name, "extrude")
}

/// Revolves a 2D profile around the y axis. `profile` rings are
/// [radius, y] points (radius >= 0); `degrees` < 360 leaves an open wedge
/// with flat end caps. `segments` 0 picks a smooth default.
pub fn revolve(
    name: &str,
    profile: &[Vec<[f32; 2]>],
    degrees: f32,
    segments: u32,
    round: f32,
) -> Result<Part, ModelerError> {
    check_profile(profile)?;
    // Arcs that end on the axis land a rounding error past it (an arc to 90°
    // gives radius -1.3e-7): snap those to 0, reject real negatives.
    let scale = profile
        .iter()
        .flatten()
        .map(|p| p[0].abs().max(p[1].abs()))
        .fold(0.0f32, f32::max);
    let snap = (scale * 1e-5).max(1e-6);
    if let Some(p) = profile.iter().flatten().find(|p| p[0] < -snap) {
        return Err(ModelerError::Csg(format!(
            "revolve `{name}`: profile points are [radius, y] with radius >= 0, got {p:?}"
        )));
    }
    let snapped: Vec<Vec<[f32; 2]>> = profile
        .iter()
        .map(|r| r.iter().map(|p| [p[0].max(0.0), p[1]]).collect())
        .collect();
    let profile = &snapped;
    if degrees.is_nan() || degrees <= 0.0 || degrees > 360.0 {
        return Err(ModelerError::Csg(format!(
            "revolve `{name}`: degrees must be in (0, 360], got {degrees}"
        )));
    }
    let rings = profile
        .iter()
        .map(|r| r.iter().map(|p| [p[0] as f64, p[1] as f64]).collect())
        .collect();
    // Rounding can push points past the axis; clip them back to radius >= 0.
    let cs = round_cross_section(cross_section(rings), round);
    let cs = if round > 0.0 {
        let clipped: Vec<Vec<[f64; 2]>> = cs
            .to_polygons()
            .into_iter()
            .map(|r| r.into_iter().map(|p| [p[0].max(0.0), p[1]]).collect())
            .collect();
        cross_section(clipped)
    } else {
        cs
    };
    let segs = if segments == 0 { 48 } else { segments.max(3) } as i32;
    let m = Manifold::revolve(&cs, segs, degrees as f64);
    let mut part = finish_sweep(&m, name, "revolve")?;
    if round > 0.0 && degrees >= 360.0 {
        soften_rounded_corners(&mut part, &cs.to_polygons(), round as f64);
    }
    Ok(part)
}

/// Soft shading across `round`ed corners of a revolved profile. The fillet
/// arcs are a run of tiny facets between two long profile segments, and the
/// area-weighted vertex normals at the arc's ends then lean on the long
/// segment, so a gently curving profile (a bowl, a vase) turns into flat
/// bands joined by a hairline. Corners that turn less than the crease angle
/// (40 degrees) give every vertex of the arc the normal halfway between the
/// two long segments, which is the shading the unrounded corner had; sharper
/// corners keep their own normals.
fn soften_rounded_corners(part: &mut Part, polys: &[Vec<[f64; 2]>], r: f64) {
    use std::collections::HashMap;
    let cos_limit = 40.0f64.to_radians().cos();
    let short = r * 0.2;
    // (radius, y, normal radial part, normal y part) of every soft-arc vertex.
    let mut soft: Vec<[f64; 4]> = Vec::new();
    for ring in polys {
        let n = ring.len();
        if n < 4 {
            continue;
        }
        let edge = |i: usize| {
            let (a, b) = (ring[i % n], ring[(i + 1) % n]);
            let d = [b[0] - a[0], b[1] - a[1]];
            let l = d[0].hypot(d[1]);
            (
                l,
                if l > 0.0 {
                    [d[1] / l, -d[0] / l]
                } else {
                    [0.0; 2]
                },
            )
        };
        let is_short: Vec<bool> = (0..n).map(|i| edge(i).0 < short).collect();
        if is_short.iter().all(|&s| s) || !is_short.iter().any(|&s| s) {
            continue;
        }
        // Start at a long edge so runs never wrap.
        let Some(first) = (0..n).find(|&i| !is_short[i]) else {
            continue;
        };
        let mut i = 0;
        while i < n {
            let e = (first + i) % n;
            if !is_short[e] {
                i += 1;
                continue;
            }
            let start = i;
            while i < n && is_short[(first + i) % n] {
                i += 1;
            }
            // Short edges start..i; long edges first+start-1 and first+i.
            let na = edge(first + start + n - 1).1;
            let nb = edge(first + i).1;
            if na[0] * nb[0] + na[1] * nb[1] < cos_limit {
                continue;
            }
            let m = [na[0] + nb[0], na[1] + nb[1]];
            let l = m[0].hypot(m[1]);
            if l < 1e-9 {
                continue;
            }
            // Vertices from the end of the first long edge to the start of the next.
            for k in start..=i {
                let v = ring[(first + k) % n];
                soft.push([v[0], v[1], m[0] / l, m[1] / l]);
            }
        }
    }
    if soft.is_empty() {
        return;
    }
    let cell = (r * 0.05).max(1e-6);
    let key = |a: f64, b: f64| ((a / cell).floor() as i64, (b / cell).floor() as i64);
    let mut grid: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (k, s) in soft.iter().enumerate() {
        grid.entry(key(s[0], s[1])).or_default().push(k);
    }
    let eps = cell * 0.5;
    for (v, n) in part.verts.iter().zip(part.normals.iter_mut()) {
        let (rad, y) = ((v[0] as f64).hypot(v[2] as f64), v[1] as f64);
        if rad < 1e-9 {
            continue;
        }
        let (kx, ky) = key(rad, y);
        let mut hit = None;
        'find: for dx in -1..=1 {
            for dy in -1..=1 {
                for &k in grid.get(&(kx + dx, ky + dy)).into_iter().flatten() {
                    let s = soft[k];
                    if (s[0] - rad).abs() < eps && (s[1] - y).abs() < eps {
                        hit = Some(s);
                        break 'find;
                    }
                }
            }
        }
        let Some(s) = hit else { continue };
        let want = Vec3::new(
            (s[2] * v[0] as f64 / rad) as f32,
            s[3] as f32,
            (s[2] * v[2] as f64 / rad) as f32,
        );
        // Only soften: never override a crease the mesh really has.
        if want.dot(Vec3::from(*n)) >= cos_limit as f32 {
            *n = want.to_array();
        }
    }
}

/// Rounds every corner of a 2D region by `r` (concave and convex): grow,
/// shrink twice as far, grow back, all with round joins.
fn round_cross_section(cs: manifold_csg::CrossSection, r: f32) -> manifold_csg::CrossSection {
    if r <= 0.0 {
        return cs;
    }
    let r = r as f64;
    let j = manifold_csg::JoinType::Round;
    // 64 segments per full turn: the automatic count is coarse at small radii.
    cs.offset(r, j, 2.0, 64)
        .offset(-2.0 * r, j, 2.0, 64)
        .offset(r, j, 2.0, 64)
}

/// A profile's rings after optional corner rounding (for sweep, which
/// builds its own rings).
pub(crate) fn rounded_rings(profile: &[Vec<[f32; 2]>], round: f32) -> Vec<Vec<[f64; 2]>> {
    let rings: Vec<Vec<[f64; 2]>> = profile
        .iter()
        .map(|r| r.iter().map(|p| [p[0] as f64, p[1] as f64]).collect())
        .collect();
    if round <= 0.0 {
        return rings;
    }
    round_cross_section(cross_section(rings), round).to_polygons()
}

/// Sweeps a closed 2D `profile` ([u, v] around the path, u along the
/// frame's normal, v along its binormal) along a 3D polyline `path`, with
/// rotation-minimising frames. `closed` joins the path end to its start (a
/// ring) instead of capping both ends.
pub fn sweep(
    name: &str,
    profile: &[[f32; 2]],
    path: &[[f32; 3]],
    closed: bool,
    round: f32,
    twist: f32,
    scale_end: f32,
) -> Result<Part, ModelerError> {
    check_profile(&[profile.to_vec()])?;
    if scale_end.is_nan() || scale_end <= 0.0 || !twist.is_finite() {
        return Err(ModelerError::Csg(format!(
            "sweep `{name}`: scale_end must be > 0 and twist finite"
        )));
    }
    let min_pts = if closed { 3 } else { 2 };
    if path.len() < min_pts {
        return Err(ModelerError::Csg(format!(
            "sweep `{name}`: path needs at least {min_pts} points, got {}",
            path.len()
        )));
    }
    let ring2: Vec<[f64; 2]> = match rounded_rings(&[profile.to_vec()], round).as_slice() {
        [one] => one.clone(),
        _ => {
            return Err(ModelerError::Csg(format!(
                "sweep `{name}`: the profile must be one simple ring (round may have split it)"
            )));
        }
    };
    let pts: Vec<Vec3> = path.iter().map(|p| Vec3::from(*p)).collect();
    if pts.windows(2).any(|w| (w[1] - w[0]).length() < 1e-9) {
        return Err(ModelerError::Csg(format!(
            "sweep `{name}`: path has repeated points"
        )));
    }
    let n = pts.len();
    // Tangents: bisectors at interior points (wrapping when closed).
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
            (Some(a), Some(b)) => (a.normalize() + b.normalize()).normalize_or(b.normalize()),
            (Some(a), None) => a.normalize(),
            (None, Some(b)) => b.normalize(),
            (None, None) => Vec3::Z,
        }
    };
    // Rotation-minimising frames by parallel transport.
    let t0 = tangent(0);
    let seed = if t0.y.abs() < 0.9 { Vec3::Y } else { Vec3::Z };
    let mut normal = (seed - t0 * seed.dot(t0)).normalize();
    let mut frames = Vec::with_capacity(n);
    let mut prev_t = t0;
    for i in 0..n {
        let t = tangent(i);
        let rot = bevy::math::Quat::from_rotation_arc(prev_t, t);
        normal = (rot * normal - t * (rot * normal).dot(t)).normalize_or(normal);
        // Scale the profile at bends so the wall keeps its thickness.
        let miter = if i > 0 && (i + 1 < n || closed) {
            let a = (pts[i] - pts[if i == 0 { n - 1 } else { i - 1 }]).normalize();
            1.0 / a.dot(t).clamp(0.3, 1.0)
        } else {
            1.0
        };
        frames.push((t, normal, t.cross(normal), miter));
        prev_t = t;
    }
    let m = ring2.len();
    // Fraction of the length at each point, for twist and scale_end.
    let mut along = vec![0.0f32];
    for w in pts.windows(2) {
        along.push(along[along.len() - 1] + (w[1] - w[0]).length());
    }
    let total = along[along.len() - 1].max(1e-9);
    let mut part = Part::new(name);
    for (i, p) in pts.iter().enumerate() {
        let (t, nrm, bin, miter) = frames[i];
        let f = along[i] / total;
        let (sn, cs) = (twist * f).to_radians().sin_cos();
        let k = 1.0 + (scale_end - 1.0) * f;
        for q in &ring2 {
            let (u0, v0) = (q[0] as f32 * k, q[1] as f32 * k);
            let (u, v) = (u0 * cs - v0 * sn, u0 * sn + v0 * cs);
            // Miter scaling only along the bend (the component along the
            // previous segment's direction within the profile plane).
            let off = nrm * u + bin * v;
            let a = if i > 0 {
                (pts[i] - pts[i - 1]).normalize()
            } else {
                t
            };
            let bend = (a - t * a.dot(t)).normalize_or_zero();
            let along = off.dot(bend);
            let pos = *p + off + bend * along * (miter - 1.0);
            part.verts.push(pos.to_array());
        }
    }
    let segs = if closed { n } else { n - 1 };
    for i in 0..segs {
        let (a, b) = (i * m, ((i + 1) % n) * m);
        for j in 0..m {
            let k = (j + 1) % m;
            let (p0, p1, p2, p3) = (
                (a + j) as u32,
                (a + k) as u32,
                (b + k) as u32,
                (b + j) as u32,
            );
            part.indices.extend([p0, p1, p2, p0, p2, p3]);
        }
    }
    if !closed {
        let tris = manifold_csg::triangulate_polygons(std::slice::from_ref(&ring2), 1e-9)
            .ok_or_else(|| {
                ModelerError::Csg(format!("sweep `{name}`: could not triangulate the profile"))
            })?;
        let last = ((n - 1) * m) as u32;
        for t in &tris {
            // Start cap faces back along the path, end cap forward.
            part.indices.extend([t[0], t[2], t[1]]);
            part.indices.extend([last + t[0], last + t[1], last + t[2]]);
        }
    }
    // Orientation depends on the profile's winding: make it outward.
    let vol: f32 = part
        .indices
        .as_chunks::<3>()
        .0
        .iter()
        .map(|t| {
            let [a, b, c] = t.map(|i| Vec3::from(part.verts[i as usize]));
            a.dot(b.cross(c))
        })
        .sum();
    if vol < 0.0 {
        for t in part.indices.as_chunks_mut::<3>().0 {
            t.swap(1, 2);
        }
    }
    // Through Manifold: proves it closed and gives creased normals.
    let man = part_to_manifold(&part).map_err(|e| {
        ModelerError::Csg(format!("sweep `{name}`: {e} (does the path cross itself?)"))
    })?;
    Ok(manifold_to_part(&man, name, crate::Material::default()))
}

/// Convex hull of several parts (world space), with the first's material.
pub fn hull(name: &str, parts: &[&Part]) -> Result<Part, ModelerError> {
    let mut pts = Vec::new();
    for p in parts {
        let rot = quat_of(p.rotation);
        let (sc, t) = (Vec3::from(p.scale), Vec3::from(p.translation));
        pts.extend(p.verts.iter().map(|v| {
            let w = rot * (Vec3::from(*v) * sc) + t;
            [w.x as f64, w.y as f64, w.z as f64]
        }));
    }
    let m = Manifold::hull_pts(&pts);
    if m.is_empty() || m.volume().abs() < 1e-15 {
        return Err(ModelerError::Csg(format!(
            "hull `{name}` is flat or empty (its inputs lie in a plane)"
        )));
    }
    let material = parts
        .first()
        .map(|p| p.material.clone())
        .unwrap_or_default();
    Ok(manifold_to_part(&m, name, material))
}

/// Rounds a solid's edges. `fillet`: convex edges get a radius-`r` round
/// (morphological opening with a sphere: erode by r, dilate by r), so the
/// outer size is kept. `chamfer`: the same with an octahedron, a 45° bevel on
/// axis-aligned edges. `smooth`: subdivision smoothing of edges sharper than
/// 60°, `segments` refinement steps (radius ignored).
pub fn round(
    name: &str,
    part: &Part,
    mode: &str,
    r: f32,
    segments: u32,
) -> Result<Part, ModelerError> {
    let m = part_to_manifold(part)?;
    let tris = m.num_tri();
    let out = match mode {
        "fillet" | "chamfer" if tris > 20_000 => {
            return Err(ModelerError::Csg(format!(
                "round `{name}`: `{}` has {tris} triangles; {mode} on that many would take minutes. \
                 Round the simple pieces before booleans/unions (a box is 12 triangles), \
                 lower the input's segment count, or use mode smooth",
                part.name
            )));
        }
        "fillet" | "chamfer" => {
            if r.is_nan() || r <= 0.0 {
                return Err(ModelerError::Csg(format!(
                    "round `{name}`: radius must be > 0, got {r}"
                )));
            }
            let r = r as f64;
            let tool = if mode == "fillet" {
                let segs = if segments == 0 {
                    16
                } else {
                    segments.clamp(4, 64)
                } as i32;
                Manifold::sphere(r, segs)
            } else {
                Manifold::hull_pts(&[
                    [r, 0.0, 0.0],
                    [-r, 0.0, 0.0],
                    [0.0, r, 0.0],
                    [0.0, -r, 0.0],
                    [0.0, 0.0, r],
                    [0.0, 0.0, -r],
                ])
            };
            let eroded = m.minkowski_difference(&tool);
            if eroded.is_empty() {
                let (lo, hi) = part.bounds();
                let d = hi - lo;
                return Err(ModelerError::Csg(format!(
                    "round `{name}`: radius {r} is larger than half of `{}`'s thinnest section \
                     (its box is {:.4} x {:.4} x {:.4} m, and walls can be thinner); use a smaller radius",
                    part.name, d.x, d.y, d.z
                )));
            }
            eroded.minkowski_sum(&tool)
        }
        "smooth" => {
            let n = if segments == 0 {
                3
            } else {
                segments.clamp(1, 8)
            } as i32;
            m.smooth_out(60.0, 0.0).refine(n)
        }
        other => {
            return Err(ModelerError::Csg(format!(
                "round `{name}`: mode must be fillet, chamfer or smooth, got `{other}`"
            )));
        }
    };
    out.status()
        .map_err(|e| ModelerError::Csg(format!("round `{name}` failed: {e}")))?;
    if out.is_empty() {
        return Err(ModelerError::Csg(format!(
            "round `{name}` produced nothing"
        )));
    }
    let mut p = manifold_to_part(&out, name, part.material.clone());
    p.visible = true;
    Ok(p)
}

/// Hollows a solid to walls `t` thick: the cavity is the input eroded by a
/// ball of radius `t` (every face moved in by exactly `t`; convex corners
/// stay sharp), and each `open` face (top/bottom/left/right/front/back)
/// has the cavity swept out through that wall, leaving an opening.
pub fn shell(
    name: &str,
    part: &Part,
    t: f32,
    open: &[String],
    method: Option<&str>,
) -> Result<Part, ModelerError> {
    if t.is_nan() || t <= 0.0 {
        return Err(ModelerError::Csg(format!(
            "shell `{name}`: thickness must be > 0, got {t}"
        )));
    }
    let mut dirs = Vec::new();
    for w in open {
        dirs.push(match w.as_str() {
            "top" => [0.0, 1.0, 0.0],
            "bottom" => [0.0, -1.0, 0.0],
            "right" => [1.0, 0.0, 0.0],
            "left" => [-1.0, 0.0, 0.0],
            "front" => [0.0, 0.0, 1.0],
            "back" => [0.0, 0.0, -1.0],
            other => {
                return Err(ModelerError::Csg(format!(
                    "shell `{name}`: open: `{other}` is not a face; use top, bottom, left, right, front or back"
                )));
            }
        });
    }
    let m = part_to_manifold(part)?;
    let tris = m.num_tri();
    let offset = match method {
        Some("offset") => true,
        Some(_) => false,
        None => tris > SHELL_EXACT_TRIS,
    };
    if offset {
        return shell_offset(name, part, &m, t, open);
    }
    if tris > SHELL_EXACT_TRIS {
        return Err(ModelerError::Csg(format!(
            "shell `{name}`: `{}` has {tris} triangles, too many for method exact (at most {SHELL_EXACT_TRIS}); use method offset, or a simpler input",
            part.name
        )));
    }
    let t = f64::from(t);
    // 16 segments puts sphere vertices on the axes, so axis-aligned walls
    // come out exactly `t` thick.
    let ball = Manifold::sphere(t, 16);
    let mut cavity = m.minkowski_difference(&ball);
    if cavity.is_empty() || cavity.volume().abs() < 1e-15 {
        let (lo, hi) = part.bounds();
        let d = hi - lo;
        return Err(ModelerError::Csg(format!(
            "shell `{name}`: thickness {t} leaves no inside: `{}` is {:.4} x {:.4} x {:.4} m, so walls must be under half its thinnest section",
            part.name, d.x, d.y, d.z
        )));
    }
    for d in dirs {
        // Sweep the cavity 2t outward through that wall (a thin prism as the
        // Minkowski tool, so concave cavities sweep correctly too).
        let e = t * 1e-6;
        let mut pts = Vec::new();
        for k in [0.0, 2.0 * t] {
            for sx in [-e, e] {
                for sy in [-e, e] {
                    for sz in [-e, e] {
                        pts.push([d[0] * k + sx, d[1] * k + sy, d[2] * k + sz]);
                    }
                }
            }
        }
        cavity = cavity.minkowski_sum(&Manifold::hull_pts(&pts));
    }
    let out = m.difference(&cavity);
    out.status()
        .map_err(|e| ModelerError::Csg(format!("shell `{name}` failed: {e}")))?;
    if out.is_empty() {
        return Err(ModelerError::Csg(format!(
            "shell `{name}` produced nothing"
        )));
    }
    let mut p = manifold_to_part(&out, name, part.material.clone());
    p.visible = true;
    Ok(p)
}

/// Most triangles `shell` erodes exactly (a ball Minkowski difference).
const SHELL_EXACT_TRIS: usize = 20_000;

/// `shell` by an inward offset surface: the solid minus itself moved in by
/// `t` along its vertex normals ([`crate::offset`]). Sharp convex edges
/// round toward the inside; a bend tighter than `t` would fold the inner
/// surface, which the wall's volume catches.
fn shell_offset(
    name: &str,
    part: &Part,
    m: &Manifold,
    t: f32,
    open: &[String],
) -> Result<Part, ModelerError> {
    let err = |s: String| ModelerError::Csg(format!("shell `{name}`: {s}"));
    if !open.is_empty() {
        return Err(err(
            "open needs method exact; with offset, cut the opening with a boolean afterwards"
                .into(),
        ));
    }
    let inner = inner_offset(m, t).map_err(|e| {
        let (lo, hi) = part.bounds();
        let d = hi - lo;
        err(format!(
            "{e}: `{}` is {:.3} x {:.3} x {:.3} m and bends tighter than {t} somewhere; use a thinner wall",
            part.name, d.x, d.y, d.z
        ))
    })?;
    let out = m.difference(&inner);
    out.status().map_err(|e| err(format!("failed: {e}")))?;
    if out.is_empty() {
        return Err(err("produced nothing".into()));
    }
    let mut p = manifold_to_part(&out, name, part.material.clone());
    p.visible = true;
    Ok(p)
}

/// The solid `m` moved in by `t` along its vertex normals, checked: a wall
/// whose volume is far from area x t means the inner surface folded.
fn inner_offset(m: &Manifold, t: f32) -> Result<Manifold, String> {
    let mesh = m.to_meshgl64();
    let n = mesh.num_prop().max(3);
    let v: Vec<Vec3> = mesh
        .vert_properties()
        .chunks(n)
        .map(|p| Vec3::new(p[0] as f32, p[1] as f32, p[2] as f32))
        .collect();
    let idx: Vec<u32> = mesh.tri_verts().iter().map(|&i| i as u32).collect();
    let moved = crate::offset::offset_positions(&v, &idx, -t);
    let mut inner = Part::new("inner");
    inner.verts = moved.iter().map(|p| p.to_array()).collect();
    inner.indices = idx;
    let inner = part_to_manifold(&inner).map_err(|e| format!("the inner surface: {e}"))?;
    let (outer_v, inner_v) = (m.volume(), inner.volume());
    let want = m.surface_area() * f64::from(t);
    let wall = outer_v - inner_v;
    if inner_v <= 0.0 || wall < 0.5 * want || wall > 1.5 * want {
        return Err(format!(
            "thickness {t} folds the inner surface over itself (walls hold {wall:.3e} m3 where {want:.3e} was expected)"
        ));
    }
    Ok(inner)
}

/// Splits `part` by the closed solid `cutter` into `name_inside` and
/// `name_outside` in one Manifold split, so the two share one cut boundary
/// and no surface of either lies on the other. `depth` > 0 keeps only that
/// skin under the input's surface inside; `lift` moves the inside piece
/// out along the mean normal of its outer surface (the input's own
/// triangles in it).
pub fn split_by(
    name: &str,
    part: &Part,
    cutter: &Part,
    depth: f32,
    lift: f32,
) -> Result<(Part, Part), ModelerError> {
    let err = |s: String| ModelerError::Csg(format!("split `{name}`: {s}"));
    let a = part_to_manifold(part)?.as_original();
    let id = a.original_id();
    let mut c =
        part_to_manifold(cutter).map_err(|e| err(format!("cutter `{}`: {e}", cutter.name)))?;
    if depth > 0.0 {
        let inner = inner_offset(&a, depth).map_err(|e| err(format!("depth: {e}")))?;
        c = c.difference(&inner);
    }
    let (ins, outs) = a.split(&c);
    ins.status().map_err(|e| err(format!("failed: {e}")))?;
    if ins.is_empty() || ins.volume().abs() < 1e-15 {
        return Err(err(format!(
            "the cutter `{}` misses `{}` (nothing inside it)",
            cutter.name, part.name
        )));
    }
    if outs.is_empty() || outs.volume().abs() < 1e-15 {
        return Err(err(format!(
            "the cutter `{}` swallows all of `{}` (nothing outside it)",
            cutter.name, part.name
        )));
    }
    let mut inside = manifold_to_part(&ins, &format!("{name}_inside"), part.material.clone());
    let mut outside = manifold_to_part(&outs, &format!("{name}_outside"), part.material.clone());
    inside.visible = true;
    outside.visible = true;
    if lift != 0.0 {
        let mesh = ins.to_meshgl64();
        let n = mesh.num_prop().max(3);
        let vp = mesh.vert_properties();
        let pt = |i: u64| {
            let k = i as usize * n;
            bevy::math::DVec3::new(vp[k], vp[k + 1], vp[k + 2])
        };
        let tv = mesh.tri_verts();
        let (runs, ids) = (mesh.run_index(), mesh.run_original_id());
        let mut dir = bevy::math::DVec3::ZERO;
        for r in 0..ids.len().min(runs.len().saturating_sub(1)) {
            if ids[r] as i32 != id {
                continue;
            }
            let (s0, s1) = (runs[r] as usize, runs[r + 1] as usize);
            for t in tv[s0..s1.min(tv.len())].as_chunks::<3>().0 {
                let (p0, p1, p2) = (pt(t[0]), pt(t[1]), pt(t[2]));
                dir += (p1 - p0).cross(p2 - p0);
            }
        }
        let dir = dir.normalize_or_zero().as_vec3();
        if dir == Vec3::ZERO {
            return Err(err(
                "lift: the inside piece has none of the input's surface to lift along".into(),
            ));
        }
        inside.translation = (Vec3::from(inside.translation) + dir * lift).to_array();
    }
    Ok((inside, outside))
}

/// Points on a sphere of radius `r` about `c` (`n` arc steps per quarter).
fn sphere_pts(out: &mut Vec<[f64; 3]>, c: [f64; 3], r: f64, n: u32) {
    let n = n.max(1);
    let around = 4 * n;
    for j in 0..=2 * n {
        let lat = std::f64::consts::PI * (j as f64 / (2 * n) as f64 - 0.5);
        for i in 0..around {
            let lon = std::f64::consts::TAU * i as f64 / around as f64;
            out.push([
                c[0] + r * lat.cos() * lon.cos(),
                c[1] + r * lat.sin(),
                c[2] + r * lat.cos() * lon.sin(),
            ]);
        }
    }
}

/// A box, cylinder or wedge with its edges rounded by `r` (same outer size
/// as the sharp primitive, centred the same way): the convex hull of spheres
/// (box, wedge) or rim rings (cylinder) inset by `r`. A closed solid.
pub fn rounded_primitive(
    name: &str,
    kind: crate::PrimitiveKind,
    size: [f32; 3],
    r: f32,
    segments: u32,
) -> Result<Part, ModelerError> {
    use crate::PrimitiveKind as K;
    let n = if segments == 0 {
        4
    } else {
        segments.clamp(1, 32)
    };
    let rr = f64::from(r);
    let s = size.map(f64::from);
    let err = |thin: f64| {
        ModelerError::Csg(format!(
            "primitive `{name}`: round {r} must be under half its thinnest size ({thin:.4} m)"
        ))
    };
    let mut pts = Vec::new();
    match kind {
        K::Box => {
            let thin = s[0].min(s[1]).min(s[2]);
            if rr * 2.0 >= thin {
                return Err(err(thin));
            }
            let h = [s[0] / 2.0 - rr, s[1] / 2.0 - rr, s[2] / 2.0 - rr];
            for sx in [-1.0, 1.0] {
                for sy in [-1.0, 1.0] {
                    for sz in [-1.0, 1.0] {
                        sphere_pts(&mut pts, [sx * h[0], sy * h[1], sz * h[2]], rr, n);
                    }
                }
            }
        }
        K::Cylinder => {
            let (rad, hh) = (s[0], s[1]);
            if rr >= rad || rr * 2.0 >= hh {
                return Err(err((2.0 * rad).min(hh)));
            }
            let around = 32.max(8 * n);
            for i in 0..around {
                let t = std::f64::consts::TAU * i as f64 / around as f64;
                for k in 0..=n {
                    let phi = std::f64::consts::FRAC_PI_2 * k as f64 / n as f64;
                    let rad_k = rad - rr + rr * phi.cos();
                    let dy = hh / 2.0 - rr + rr * phi.sin();
                    for y in [dy, -dy] {
                        pts.push([rad_k * t.cos(), y, rad_k * t.sin()]);
                    }
                }
            }
        }
        K::Wedge => {
            let (w, h, d) = (s[0], s[1], s[2]);
            let tri = [
                [-w / 2.0, -h / 2.0],
                [w / 2.0, -h / 2.0],
                [-w / 2.0, h / 2.0],
            ];
            // Inradius of the right triangle.
            let inr = (w + h - (w * w + h * h).sqrt()) / 2.0;
            if rr >= inr || rr * 2.0 >= d {
                return Err(err((2.0 * inr).min(d)));
            }
            for k in 0..3 {
                let v = tri[k];
                let a = tri[(k + 1) % 3];
                let b = tri[(k + 2) % 3];
                let unit = |p: [f64; 2]| {
                    let (x, y) = (p[0] - v[0], p[1] - v[1]);
                    let l = (x * x + y * y).sqrt();
                    [x / l, y / l]
                };
                let (u1, u2) = (unit(a), unit(b));
                let bis = [u1[0] + u2[0], u1[1] + u2[1]];
                let bl = (bis[0] * bis[0] + bis[1] * bis[1]).sqrt();
                let bis = [bis[0] / bl, bis[1] / bl];
                let sin_half = (u1[0] * bis[1] - u1[1] * bis[0]).abs();
                let c = [v[0] + bis[0] * rr / sin_half, v[1] + bis[1] * rr / sin_half];
                for z in [d / 2.0 - rr, rr - d / 2.0] {
                    sphere_pts(&mut pts, [c[0], c[1], z], rr, n);
                }
            }
        }
        other => {
            return Err(ModelerError::Csg(format!(
                "primitive `{name}`: round works on box, cylinder and wedge, not {other} \
                 (spheres, capsules and tori are already round; use a round op for others)"
            )));
        }
    }
    let m = Manifold::hull_pts(&pts);
    m.status()
        .map_err(|e| ModelerError::Csg(format!("primitive `{name}` failed: {e}")))?;
    let mut p = manifold_to_part(&m, name, crate::Material::default());
    p.visible = true;
    crate::uv::ensure_uvs(&mut p, Some("box"))?;
    Ok(p)
}

/// Opening (erode, then dilate) by `tool`; errors when nothing survives.
fn open_by(
    name: &str,
    part: &Part,
    m: &Manifold,
    tool: &Manifold,
    r: f32,
) -> Result<Manifold, ModelerError> {
    let eroded = m.minkowski_difference(tool);
    if eroded.is_empty() {
        let (lo, hi) = part.bounds();
        let d = hi - lo;
        return Err(ModelerError::Csg(format!(
            "round `{name}`: radius {r} is larger than half of `{}`'s thinnest section \
             (its box is {:.4} x {:.4} x {:.4} m); use a smaller radius",
            part.name, d.x, d.y, d.z
        )));
    }
    Ok(eroded.minkowski_sum(tool))
}

/// Fillet only some edge classes of a solid: `top` / `bottom` (horizontal
/// edges in the upper / lower half of its box), `horizontal` (both),
/// `vertical` (edges along y). Built from openings with direction-limited
/// tools (a y cylinder rounds only vertical edges, the intersection of x and
/// z cylinders only horizontal ones), the upper and lower halves joined at
/// the box's mid-height. Exact for axis-aligned edges; slanted edges are
/// rounded by whichever tool their half uses; a top/bottom split needs the
/// part to be taller than 2 x radius.
pub fn fillet_edges(
    name: &str,
    part: &Part,
    r: f32,
    segments: u32,
    edges: &[String],
) -> Result<Part, ModelerError> {
    if r.is_nan() || r <= 0.0 {
        return Err(ModelerError::Csg(format!(
            "round `{name}`: radius must be > 0, got {r}"
        )));
    }
    let (mut top, mut bottom, mut vertical) = (false, false, false);
    for e in edges {
        match e.as_str() {
            "all" => (top, bottom, vertical) = (true, true, true),
            "top" => top = true,
            "bottom" => bottom = true,
            "horizontal" => (top, bottom) = (true, true),
            "vertical" => vertical = true,
            other => {
                return Err(ModelerError::Csg(format!(
                    "round `{name}`: edges: `{other}` is not an edge class; use all, top, bottom, vertical or horizontal"
                )));
            }
        }
    }
    let m = part_to_manifold(part)?;
    let tris = m.num_tri();
    if tris > 20_000 {
        return Err(ModelerError::Csg(format!(
            "round `{name}`: `{}` has {tris} triangles; fillet the simple pieces before booleans",
            part.name
        )));
    }
    let rf = f64::from(r);
    let segs = if segments == 0 {
        16
    } else {
        segments.clamp(4, 64)
    } as i32;
    let cyl = |axis: char| {
        let c = Manifold::cylinder(2.0 * rf, rf, rf, segs, true);
        match axis {
            'x' => c.rotate(0.0, 90.0, 0.0),
            'y' => c.rotate(90.0, 0.0, 0.0),
            _ => c,
        }
    };
    let tool = |horizontal: bool, vert: bool| -> Option<Manifold> {
        match (horizontal, vert) {
            (true, true) => Some(Manifold::sphere(rf, segs)),
            (true, false) => Some(cyl('x').intersection(&cyl('z'))),
            (false, true) => Some(cyl('y')),
            (false, false) => None,
        }
    };
    let opened = |t: Option<Manifold>| -> Result<Manifold, ModelerError> {
        match t {
            Some(t) => open_by(name, part, &m, &t, r),
            None => Ok(m.clone()),
        }
    };
    let out = if top == bottom {
        opened(tool(top, vertical))?
    } else {
        let (lo, hi) = part.bounds();
        let (lo, hi) = (lo.as_dvec3(), hi.as_dvec3());
        let cy = (lo.y + hi.y) / 2.0;
        if hi.y - lo.y <= 2.0 * rf {
            return Err(ModelerError::Csg(format!(
                "round `{name}`: edges top/bottom needs `{}` taller than 2 x radius ({:.4} m tall)",
                part.name,
                hi.y - lo.y
            )));
        }
        let pad = (hi - lo).max_element() + 1.0;
        let overlap = ((hi.y - lo.y) * 1e-3).min(rf * 0.5);
        let slab = |y0: f64, y1: f64| {
            Manifold::cube(
                hi.x - lo.x + 2.0 * pad,
                y1 - y0,
                hi.z - lo.z + 2.0 * pad,
                false,
            )
            .translate(lo.x - pad, y0, lo.z - pad)
        };
        let upper = opened(tool(top, vertical))?.intersection(&slab(cy - overlap, hi.y + pad));
        let lower = opened(tool(bottom, vertical))?.intersection(&slab(lo.y - pad, cy + overlap));
        upper.union(&lower)
    };
    out.status()
        .map_err(|e| ModelerError::Csg(format!("round `{name}` failed: {e}")))?;
    if out.is_empty() {
        return Err(ModelerError::Csg(format!(
            "round `{name}` produced nothing"
        )));
    }
    let mut p = manifold_to_part(&out, name, part.material.clone());
    p.visible = true;
    Ok(p)
}

/// Recesses a `rows` x `cols` grid of panels `depth` deep into one face of a
/// solid's world box (top/bottom/left/right/front/back), `inset` in from
/// the face's edges with `gap`-wide seams left standing between them. Rows
/// run down the face (along y on the sides, along z on top/bottom), columns
/// across it. The face is the box side: flat faces flush with the part's
/// box work as expected; curved ones get cut where they reach it.
#[allow(clippy::too_many_arguments)]
pub fn panel(
    name: &str,
    part: &Part,
    face: &str,
    rows: u32,
    cols: u32,
    inset: f32,
    depth: f32,
    gap: f32,
) -> Result<Part, ModelerError> {
    // (normal axis, sign, row axis, column axis)
    let (n, sign, ra, ca) = match face {
        "top" => (1, 1.0, 2, 0),
        "bottom" => (1, -1.0, 2, 0),
        "right" => (0, 1.0, 1, 2),
        "left" => (0, -1.0, 1, 2),
        "front" => (2, 1.0, 1, 0),
        "back" => (2, -1.0, 1, 0),
        other => {
            return Err(ModelerError::Csg(format!(
                "panel `{name}`: face: `{other}` is not a face; use top, bottom, left, right, front or back"
            )));
        }
    };
    let rows = rows.max(1);
    let cols = cols.max(1);
    if depth.is_nan() || depth <= 0.0 || inset.is_nan() || inset < 0.0 || gap.is_nan() || gap < 0.0
    {
        return Err(ModelerError::Csg(format!(
            "panel `{name}`: depth must be > 0 and inset, gap >= 0 (got depth {depth}, inset {inset}, gap {gap})"
        )));
    }
    let m = part_to_manifold(part)?;
    let (lo, hi) = part.bounds();
    let (lo, hi) = (lo.as_dvec3().to_array(), hi.as_dvec3().to_array());
    let (inset, depth, gap) = (f64::from(inset), f64::from(depth), f64::from(gap));
    let thick = hi[n] - lo[n];
    if depth >= thick {
        return Err(ModelerError::Csg(format!(
            "panel `{name}`: depth {depth} cuts through `{}` ({thick:.4} m thick there)",
            part.name
        )));
    }
    let cell = |axis: usize, count: u32| -> Result<f64, ModelerError> {
        let span = hi[axis] - lo[axis] - 2.0 * inset - (count - 1) as f64 * gap;
        let c = span / count as f64;
        if c <= 0.0 {
            return Err(ModelerError::Csg(format!(
                "panel `{name}`: {count} panels with inset {inset} and gap {gap} don't fit on a {:.4} m side",
                hi[axis] - lo[axis]
            )));
        }
        Ok(c)
    };
    let (rc, cc) = (cell(ra, rows)?, cell(ca, cols)?);
    let plane = if sign > 0.0 { hi[n] } else { lo[n] };
    let out_pad = depth + 0.01 * thick.max(1e-3);
    let mut cutters = Vec::new();
    for i in 0..rows {
        for j in 0..cols {
            let mut size = [0.0; 3];
            let mut at = [0.0; 3];
            size[ra] = rc;
            at[ra] = lo[ra] + inset + i as f64 * (rc + gap);
            size[ca] = cc;
            at[ca] = lo[ca] + inset + j as f64 * (cc + gap);
            size[n] = depth + out_pad;
            at[n] = if sign > 0.0 {
                plane - depth
            } else {
                plane - out_pad
            };
            cutters.push(
                Manifold::cube(size[0], size[1], size[2], false).translate(at[0], at[1], at[2]),
            );
        }
    }
    let out = m.difference(&Manifold::batch_union(&cutters));
    out.status()
        .map_err(|e| ModelerError::Csg(format!("panel `{name}` failed: {e}")))?;
    if out.is_empty() {
        return Err(ModelerError::Csg(format!(
            "panel `{name}` produced nothing"
        )));
    }
    if (out.volume() - m.volume()).abs() < 1e-12 {
        return Err(ModelerError::Csg(format!(
            "panel `{name}`: the panels miss `{}`'s {face} face (is it flush with its box?)",
            part.name
        )));
    }
    let mut p = manifold_to_part(&out, name, part.material.clone());
    p.visible = true;
    Ok(p)
}

#[cfg(test)]
mod shaping_tests {
    use super::*;

    fn closed(p: &Part) -> Manifold {
        part_to_manifold(p).unwrap_or_else(|e| panic!("not closed: {e}"))
    }

    fn cube(n: &str, s: [f32; 3]) -> Part {
        crate::primitives::primitive(crate::PrimitiveKind::Box, s, n)
            .unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn split_by_a_cutter_shares_one_boundary() {
        // A body and a window cutter through its +x face.
        let body = cube("body", [1.0, 1.0, 2.0]);
        let mut cut = cube("win", [0.4, 0.4, 0.8]);
        cut.translation = [0.5, 0.1, 0.0];
        let (ins, outs) = split_by("b", &body, &cut, 0.0, 0.0).unwrap_or_else(|e| panic!("{e}"));
        let (mi, mo) = (closed(&ins), closed(&outs));
        assert!((mi.volume() + mo.volume() - 2.0).abs() < 1e-6);
        assert!(
            (mi.volume() - 0.2 * 0.4 * 0.8).abs() < 1e-6,
            "{}",
            mi.volume()
        );
        // Exactly partitioned: their union is the body again, and nothing
        // of one is inside the other.
        assert!(mi.intersection(&mo).volume().abs() < 1e-9);
        assert!((mi.union(&mo).volume() - 2.0).abs() < 1e-6);
        // depth: a skin 2 cm deep under the face; lift moves it out along +x.
        let (ins, _) = split_by("b", &body, &cut, 0.02, 0.003).unwrap_or_else(|e| panic!("{e}"));
        let v = closed(&ins).volume();
        assert!((v - 0.4 * 0.8 * 0.02).abs() < 0.4 * 0.8 * 0.02 * 0.1, "{v}");
        let t = Vec3::from(ins.translation);
        assert!((t - Vec3::new(0.003, 0.0, 0.0)).length() < 1e-5, "{t}");
        // A cutter that misses is an error.
        cut.translation = [3.0, 0.0, 0.0];
        let e = split_by("b", &body, &cut, 0.0, 0.0)
            .err()
            .map(|e| e.to_string());
        assert!(e.is_some_and(|e| e.contains("misses")));
    }

    #[test]
    fn rounded_primitives_are_closed_and_keep_their_size() {
        for (kind, size) in [
            (crate::PrimitiveKind::Box, [1.0, 0.5, 0.8]),
            (crate::PrimitiveKind::Cylinder, [0.3, 1.0, 0.0]),
            (crate::PrimitiveKind::Wedge, [1.0, 0.6, 0.5]),
        ] {
            let p = rounded_primitive("p", kind, size, 0.05, 0)
                .unwrap_or_else(|e| panic!("{kind}: {e}"));
            let m = closed(&p);
            let sharp_part =
                crate::primitives::primitive(kind, size, "s").unwrap_or_else(|e| panic!("{e}"));
            let sharp = closed(&sharp_part);
            assert!(m.volume() < sharp.volume() && m.volume() > 0.9 * sharp.volume());
            let (lo, hi) = p.bounds();
            let (slo, shi) = sharp_part.bounds();
            assert!((lo - slo).abs().max_element() < 1e-3, "{kind} lo");
            // The wedge's acute tips round off inside its box.
            if kind != crate::PrimitiveKind::Wedge {
                assert!((hi - shi).abs().max_element() < 1e-3, "{kind} hi");
            }
            // Booleans accept it.
            let cut = m.difference(&Manifold::cube(0.1, 0.1, 2.0, true));
            assert!(cut.status().is_ok() && cut.volume() < m.volume());
        }
        assert!(rounded_primitive("p", crate::PrimitiveKind::Box, [0.1; 3], 0.06, 0).is_err());
        assert!(rounded_primitive("p", crate::PrimitiveKind::Sphere, [0.1; 3], 0.01, 0).is_err());
    }

    #[test]
    fn fillet_edges_rounds_only_the_chosen_class() {
        let b = cube("b", [1.0, 1.0, 1.0]);
        let full = closed(&b).volume();
        let r = 0.2_f32;
        // Rounding one edge of length 1 removes (1 - pi/4) r^2.
        let per_edge = (1.0 - std::f64::consts::FRAC_PI_4) * f64::from(r * r);
        let vol = |edges: &[&str]| {
            let e: Vec<String> = edges.iter().map(|s| s.to_string()).collect();
            let p = fillet_edges("f", &b, r, 32, &e).unwrap_or_else(|e| panic!("{e}"));
            full - closed(&p).volume()
        };
        let v = vol(&["vertical"]);
        assert!((v - 4.0 * per_edge).abs() < 0.2 * per_edge, "vertical {v}");
        let h = vol(&["horizontal"]);
        // Where an x edge meets a z edge their cuts overlap a little.
        assert!(h < 8.0 * per_edge && h > 7.0 * per_edge, "horizontal {h}");
        let t = vol(&["top"]);
        assert!((t - 4.0 * per_edge).abs() < 0.3 * per_edge, "top {t}");
        assert!(vol(&["top", "vertical"]) > t + 2.0 * per_edge);
        assert!(fillet_edges("f", &b, r, 0, &["sides".to_string()]).is_err());
    }

    #[test]
    fn panel_recesses_a_grid() {
        let b = cube("wall", [2.0, 1.0, 0.2]);
        let p = panel("w", &b, "front", 2, 3, 0.05, 0.01, 0.02).unwrap_or_else(|e| panic!("{e}"));
        let m = closed(&p);
        let rc = (1.0 - 0.1 - 0.02) / 2.0;
        let cc = (2.0 - 0.1 - 0.04) / 3.0;
        let removed = 0.4 - m.volume();
        assert!((removed - 6.0 * rc * cc * 0.01).abs() < 1e-4, "{removed}");
        assert!(p.bounds().1.z > 0.099, "seams and border stay at the face");
        assert!(panel("w", &b, "front", 2, 3, 0.05, 0.3, 0.02).is_err());
        assert!(panel("w", &b, "side", 1, 1, 0.05, 0.01, 0.0).is_err());
        assert!(panel("w", &b, "top", 40, 40, 0.05, 0.01, 0.1).is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model_with_box(name: &str, half: f32) -> Model {
        let mut model = Model {
            name: "m".into(),
            ..Default::default()
        };
        let part = crate::primitives::primitive(crate::PrimitiveKind::Box, [half * 2.0; 3], name)
            .unwrap_or_else(|_| panic!("{name} failed"));
        model.parts.push(part.into());
        model
    }

    #[test]
    fn box_minus_smaller_box_has_more_triangles() {
        let mut model = model_with_box("big", 2.0);
        let small = crate::primitives::primitive(crate::PrimitiveKind::Box, [1.0; 3], "small")
            .unwrap_or_else(|_| panic!("small failed"));
        model.parts.push(small.into());
        let before = model.part("big").map_or(0, |p| p.tri_count());
        let out = match boolean(&model, crate::BoolOp::Difference, "big", "small", None) {
            Ok(out) => out,
            Err(e) => panic!("difference failed: {e}"),
        };
        assert!(
            out.tri_count() > before,
            "{} tris vs {before}",
            out.tri_count()
        );
        assert_eq!(out.name, "big-difference-small");
        // The boolean is pure: both original parts still exist on the model.
        assert!(model.part("big").is_some());
        assert!(model.part("small").is_some());
        validate(&out, "difference");
    }

    #[test]
    fn union_and_intersect_smoke() {
        let mut model = model_with_box("a", 2.0);
        model.parts.push(
            crate::primitives::primitive(crate::PrimitiveKind::Sphere, [1.0; 3], "b")
                .unwrap_or_else(|_| panic!("b failed"))
                .into(),
        );
        for op in [
            crate::BoolOp::Union,
            crate::BoolOp::Intersect,
            crate::BoolOp::Difference,
        ] {
            let out = match boolean(&model, op, "a", "b", None) {
                Ok(out) => out,
                Err(e) => panic!("{op} failed: {e}"),
            };
            validate(&out, op.as_str());
        }
    }

    #[test]
    fn disjoint_difference_errs() {
        let mut model = model_with_box("a", 1.0);
        let mut far = crate::primitives::primitive(crate::PrimitiveKind::Box, [1.0; 3], "b")
            .unwrap_or_else(|_| panic!("b failed"));
        far.translation = [100.0, 0.0, 0.0];
        model.parts.push(far.into());
        assert!(boolean(&model, crate::BoolOp::Intersect, "a", "b", None).is_err());
    }

    #[test]
    fn transform_is_applied_before_the_boolean() {
        // A small box rotated 45° cutting a bigger box still carves a hole.
        let mut model = model_with_box("big", 2.0);
        let mut cutter =
            crate::primitives::primitive(crate::PrimitiveKind::Box, [1.0; 3], "cutter")
                .unwrap_or_else(|_| panic!("cutter failed"));
        cutter.rotation = [0.0, 0.0, 0.3826834, 0.9238795]; // 45° around z
        model.parts.push(cutter.into());
        let out = match boolean(
            &model,
            crate::BoolOp::Difference,
            "big",
            "cutter",
            Some("hole"),
        ) {
            Ok(out) => out,
            Err(e) => panic!("rotated subtract failed: {e}"),
        };
        assert_eq!(out.name, "hole");
        validate(&out, "rotated-subtract");
    }

    #[test]
    fn unknown_part_and_op_err() {
        let model = model_with_box("a", 1.0);
        assert!(boolean(&model, crate::BoolOp::Union, "a", "ghost", None).is_err());
        assert!("xor".parse::<crate::BoolOp>().is_err());
    }

    #[test]
    fn partial_difference_is_closed_and_clean() {
        // A corner bite: the output must be a closed manifold with no
        // degenerate triangles, inside the big box's extents.
        let mut model = model_with_box("big", 2.0);
        let mut cutter =
            crate::primitives::primitive(crate::PrimitiveKind::Box, [2.0, 1.0, 1.0], "cutter")
                .unwrap_or_else(|_| panic!("cutter failed"));
        cutter.translation = [1.0, 0.0, 1.5];
        model.parts.push(cutter.into());
        let out = match boolean(&model, crate::BoolOp::Difference, "big", "cutter", None) {
            Ok(out) => out,
            Err(e) => panic!("partial difference failed: {e}"),
        };
        assert_eq!(crate::verify::open_edge_count(&out), 0, "open edges");
        assert!(out.tri_count() > 0, "result must be non-empty");
        for tri in out.indices.as_chunks::<3>().0 {
            let (va, vb, vc) = (
                Vec3::from(out.verts[tri[0] as usize]),
                Vec3::from(out.verts[tri[1] as usize]),
                Vec3::from(out.verts[tri[2] as usize]),
            );
            assert!(
                (vb - va).cross(vc - va).length() >= 1e-9,
                "degenerate triangle {tri:?}"
            );
        }
        let (min, max) = out.bounds();
        assert!(min.x > -2.0 - 1e-5 && max.x < 2.0 + 1e-5);
        assert!(min.y > -2.0 - 1e-5 && max.y < 2.0 + 1e-5);
        assert!(min.z > -2.0 - 1e-5 && max.z < 2.0 + 1e-5);
        validate(&out, "partial-difference");
    }

    /// Curved-on-curved used to come out as noise under csgrs.
    #[test]
    fn curved_on_curved_is_manifold() {
        let mut model = Model::named("m");
        let mk = |kind, size, t: [f32; 3], r: [f32; 4], name: &str| {
            let mut p =
                crate::primitives::primitive(kind, size, name).unwrap_or_else(|e| panic!("{e}"));
            p.translation = t;
            p.rotation = r;
            p
        };
        let id = [0.0, 0.0, 0.0, 1.0];
        let x90 = [0.70710677, 0.0, 0.0, 0.70710677];
        model.parts.push(
            mk(
                crate::PrimitiveKind::Cylinder,
                [1.0, 2.0, 0.0],
                [0.0; 3],
                id,
                "c1",
            )
            .into(),
        );
        model.parts.push(
            mk(
                crate::PrimitiveKind::Cylinder,
                [0.4, 3.0, 0.0],
                [0.0; 3],
                x90,
                "c2",
            )
            .into(),
        );
        model.parts.push(
            mk(
                crate::PrimitiveKind::Sphere,
                [1.2, 0.0, 0.0],
                [0.0; 3],
                id,
                "s",
            )
            .into(),
        );
        model.parts.push(
            mk(
                crate::PrimitiveKind::Torus,
                [1.0, 0.3, 0.0],
                [0.0; 3],
                id,
                "t",
            )
            .into(),
        );
        for (a, b) in [("c1", "c2"), ("s", "c1"), ("c1", "t"), ("s", "t")] {
            for op in [BoolOp::Difference, BoolOp::Union, BoolOp::Intersect] {
                let out = boolean(&model, op, a, b, None)
                    .unwrap_or_else(|e| panic!("{op}({a},{b}): {e}"));
                // Closed, and a valid solid for the next boolean (chained cuts).
                assert_eq!(
                    crate::verify::open_edge_count(&out),
                    0,
                    "{op}({a},{b}) has open edges"
                );
                part_to_manifold(&out).unwrap_or_else(|e| panic!("{op}({a},{b}) re-import: {e}"));
                validate(&out, op.as_str());
            }
        }
    }

    #[test]
    fn open_sheet_operand_errs_cleanly() {
        let mut model = model_with_box("a", 1.0);
        model.parts.push(
            crate::primitives::primitive(crate::PrimitiveKind::Disc, [0.5, 0.0, 0.0], "d")
                .unwrap_or_else(|e| panic!("{e}"))
                .into(),
        );
        let err = match boolean(&model, BoolOp::Difference, "a", "d", None) {
            Ok(_) => panic!("disc operand should error"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("closed solid"), "{err}");
    }

    /// UVs ride through the boolean: the untouched faces of a box keep their
    /// own UV corners instead of being box-reprojected.
    #[test]
    fn input_uvs_survive_the_boolean() {
        let mut model = model_with_box("big", 2.0);
        let mut cutter = crate::primitives::primitive(crate::PrimitiveKind::Box, [1.0; 3], "c")
            .unwrap_or_else(|e| panic!("{e}"));
        cutter.translation = [0.0, 0.0, 2.0]; // pocket into the +z face only
        model.parts.push(cutter.into());
        let big = model.part("big").unwrap_or_else(|| panic!("big"));
        // UV of the -z face's corner (-2,-2,-2) in the input.
        let want: Vec<[f32; 2]> = big
            .verts
            .iter()
            .zip(&big.uvs)
            .zip(&big.normals)
            .filter(|((v, _), n)| {
                (Vec3::from(**v) - Vec3::splat(-2.0)).length() < 1e-4 && n[2] < -0.5
            })
            .map(|((_, uv), _)| *uv)
            .collect();
        assert!(!want.is_empty());
        let out =
            boolean(&model, BoolOp::Difference, "big", "c", None).unwrap_or_else(|e| panic!("{e}"));
        let got: Vec<[f32; 2]> = out
            .verts
            .iter()
            .zip(&out.uvs)
            .zip(&out.normals)
            .filter(|((v, _), n)| {
                (Vec3::from(**v) - Vec3::splat(-2.0)).length() < 1e-4 && n[2] < -0.5
            })
            .map(|((_, uv), _)| *uv)
            .collect();
        for uv in &want {
            assert!(
                got.iter()
                    .any(|g| (g[0] - uv[0]).abs() < 1e-4 && (g[1] - uv[1]).abs() < 1e-4),
                "input uv {uv:?} lost; got {got:?}"
            );
        }
    }

    fn validate(part: &Part, what: &str) {
        assert_eq!(part.uvs.len(), part.verts.len(), "{what} uv count");
        assert_eq!(part.normals.len(), part.verts.len(), "{what} normal count");
        assert!(
            part.indices
                .iter()
                .all(|i| (*i as usize) < part.verts.len())
        );
        for uv in &part.uvs {
            assert!(
                (0.0..=1.0).contains(&uv[0]),
                "{what} u out of range: {uv:?}"
            );
            assert!(
                (0.0..=1.0).contains(&uv[1]),
                "{what} v out of range: {uv:?}"
            );
        }
    }

    /// Two cutters of equal depth into one wall used to make csgrs drop the
    /// first pocket's end face (coplanar but disjoint faces). Manifold handles
    /// it exactly, with no nudge.
    #[test]
    fn equal_depth_cutters_keep_both_pockets() {
        let mut model = Model::named("m");
        let mk = |kind, size, t: [f32; 3], name: &str| {
            let mut p =
                crate::primitives::primitive(kind, size, name).unwrap_or_else(|e| panic!("{e}"));
            p.translation = t;
            p
        };
        model.parts.push(
            mk(
                crate::PrimitiveKind::Cylinder,
                [0.8, 3.0, 0.0],
                [0.0, 1.7, 0.0],
                "tower",
            )
            .into(),
        );
        model.parts.push(
            mk(
                crate::PrimitiveKind::Box,
                [0.5, 1.0, 0.7],
                [0.0, 0.7, 0.8],
                "door",
            )
            .into(),
        );
        model.parts.push(
            mk(
                crate::PrimitiveKind::Box,
                [0.3, 0.4, 0.7],
                [0.0, 2.5, 0.8],
                "win",
            )
            .into(),
        );
        let d1 = boolean(&model, BoolOp::Difference, "tower", "door", Some("d1"))
            .unwrap_or_else(|e| panic!("{e}"));
        model.parts.push(d1.into());
        let out = boolean(&model, BoolOp::Difference, "d1", "win", Some("o"))
            .unwrap_or_else(|e| panic!("{e}"));
        // Ray down -z through each pocket must hit the pocket's end face
        // (z ~ 0.45) before the tower's back wall (z = -0.8).
        for y in [0.7f32, 2.5] {
            let first = first_hit_z(&out, y);
            assert!(
                first.is_some_and(|z| (z - 0.45).abs() < 0.02),
                "pocket at y={y} is open: first hit z = {first:?}"
            );
        }
    }

    /// z of the first triangle hit by a ray from (0, y, 2.5) along -z.
    fn first_hit_z(part: &Part, y: f32) -> Option<f32> {
        let (o, d) = (Vec3::new(0.0, y, 2.5), Vec3::new(0.0, 0.0, -1.0));
        let mut best: Option<f32> = None;
        for t in part.indices.as_chunks::<3>().0 {
            let p: Vec<Vec3> = t
                .iter()
                .map(|&i| Vec3::from(part.verts[i as usize]))
                .collect();
            let (e1, e2) = (p[1] - p[0], p[2] - p[0]);
            let h = d.cross(e2);
            let a = e1.dot(h);
            if a.abs() < 1e-9 {
                continue;
            }
            let f = 1.0 / a;
            let s = o - p[0];
            let u = f * s.dot(h);
            let q = s.cross(e1);
            let v = f * d.dot(q);
            if !(0.0..=1.0).contains(&u) || v < 0.0 || u + v > 1.0 {
                continue;
            }
            let tt = f * e2.dot(q);
            if tt > 1e-6 && best.is_none_or(|b| tt < b) {
                best = Some(tt);
            }
        }
        best.map(|t| o.z - t)
    }

    fn signed_volume(p: &Part) -> f64 {
        p.indices
            .as_chunks::<3>()
            .0
            .iter()
            .map(|t| {
                let [a, b, c] = t.map(|i| Vec3::from(p.verts[i as usize]).as_dvec3());
                a.dot(b.cross(c)) / 6.0
            })
            .sum()
    }

    fn bounds(p: &Part) -> (Vec3, Vec3) {
        p.verts.iter().fold((Vec3::MAX, Vec3::MIN), |(lo, hi), v| {
            (lo.min(Vec3::from(*v)), hi.max(Vec3::from(*v)))
        })
    }

    #[test]
    fn extrude_square_with_hole_is_a_closed_upright_tube() {
        let outer = vec![[-1.0, -1.0], [1.0, -1.0], [1.0, 1.0], [-1.0, 1.0]];
        // Clockwise inner ring: even-odd makes it a hole regardless.
        let inner = vec![[-0.5, -0.5], [-0.5, 0.5], [0.5, 0.5], [0.5, -0.5]];
        let p = extrude("tube", &[outer, inner], 2.0, 0.0, [1.0, 1.0], 0, 0.0)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(crate::verify::open_edge_count(&p), 0);
        // (4 - 1) * 2 = 6, positive = outward-wound.
        assert!(
            (signed_volume(&p) - 6.0).abs() < 1e-4,
            "{}",
            signed_volume(&p)
        );
        let (lo, hi) = bounds(&p);
        assert!(
            (lo - Vec3::new(-1.0, 0.0, -1.0)).abs().max_element() < 1e-5,
            "{lo}"
        );
        assert!(
            (hi - Vec3::new(1.0, 2.0, 1.0)).abs().max_element() < 1e-5,
            "{hi}"
        );
    }

    #[test]
    fn extrude_footprint_keeps_its_z_side() {
        // A footprint entirely at +z must stay at +z (no mirror on the swap).
        let ring = vec![[0.0, 1.0], [1.0, 1.0], [1.0, 2.0], [0.0, 2.0]];
        let p =
            extrude("f", &[ring], 0.5, 0.0, [0.0, 0.0], 0, 0.0).unwrap_or_else(|e| panic!("{e}"));
        let (lo, hi) = bounds(&p);
        assert!(
            lo.z > -1e-5 && (hi.z - 2.0).abs() < 1e-5 && hi.y < 0.51,
            "{lo} {hi}"
        );
        // Tapered to a point (the profile origin): a pyramid, 1/3 * base * h.
        assert!((signed_volume(&p) - 0.5 / 3.0).abs() < 1e-4);
    }

    #[test]
    fn revolve_snaps_rounding_past_the_axis_but_rejects_real_negatives() {
        // An arc to 90 degrees ends at radius ~ -1.3e-7.
        let dome = vec![vec![[0.0, 0.0], [3.0, 0.0], [2.12, 2.12], [-1.3e-7, 3.0]]];
        assert!(revolve("dome", &dome, 360.0, 32, 0.0).is_ok());
        let bad = vec![vec![[0.0, 0.0], [1.0, 0.0], [-0.1, 1.0]]];
        assert!(revolve("bad", &bad, 360.0, 32, 0.0).is_err());
    }

    #[test]
    fn revolve_makes_a_closed_y_axis_solid() {
        // Rectangle r 0..1, y 0..2 -> cylinder of volume ~ pi * 2.
        let ring = vec![[0.0, 0.0], [1.0, 0.0], [1.0, 2.0], [0.0, 2.0]];
        let p = revolve("cyl", std::slice::from_ref(&ring), 360.0, 64, 0.0)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(crate::verify::open_edge_count(&p), 0);
        let v = signed_volume(&p);
        assert!(v > 6.1 && v < 6.29, "{v}");
        let (lo, hi) = bounds(&p);
        assert!(lo.y.abs() < 1e-5 && (hi.y - 2.0).abs() < 1e-5, "{lo} {hi}");
        // A half turn is half the volume, still closed.
        let half = revolve("half", &[ring], 180.0, 64, 0.0).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(crate::verify::open_edge_count(&half), 0);
        assert!((signed_volume(&half) - v / 2.0).abs() < 0.05);
    }

    /// Number of positions carrying more than one distinct normal (creased rings).
    fn creased_positions(p: &Part) -> usize {
        use std::collections::HashMap;
        let mut at: HashMap<[i64; 3], Vec<Vec3>> = HashMap::new();
        for (v, n) in p.verts.iter().zip(&p.normals) {
            let k = [
                (v[0] * 1e4).round() as i64,
                (v[1] * 1e4).round() as i64,
                (v[2] * 1e4).round() as i64,
            ];
            let n = Vec3::from(*n);
            let e = at.entry(k).or_default();
            if !e.iter().any(|o| o.dot(n) > 0.99) {
                e.push(n);
            }
        }
        at.values().filter(|e| e.len() > 1).count()
    }

    #[test]
    fn revolve_round_keeps_a_smooth_profile_smooth() {
        // A bowl: a coarse quarter-ellipse (turns of ~15 degrees between points)
        // plus a short straight rim, rounded by a few millimetres.
        let mut ring = vec![[0.0, 0.0]];
        for i in 0..=6 {
            let a = (i as f32) * 15.0f32.to_radians();
            ring.push([0.05 + 0.25 * a.sin(), 0.02 + 0.2 * (1.0 - a.cos())]);
        }
        ring.push([0.3, 0.3]);
        ring.push([0.28, 0.3]);
        ring.push([0.28, 0.2]);
        ring.push([0.0, 0.2]);
        let plain = revolve("plain", std::slice::from_ref(&ring), 360.0, 12, 0.0)
            .unwrap_or_else(|e| panic!("{e}"));
        let round = revolve("round", &[ring], 360.0, 12, 0.006).unwrap_or_else(|e| panic!("{e}"));
        let steps = |p: &Part| -> Vec<f32> {
            let mut wall: Vec<(f32, Vec3)> = p
                .verts
                .iter()
                .zip(&p.normals)
                .filter(|(v, _)| v[2].abs() < 1e-5 && v[0] > 0.0 && v[1] > 0.03 && v[1] < 0.2)
                .map(|(v, n)| (v[1], Vec3::from(*n)))
                .collect();
            wall.sort_by(|a, b| a.0.total_cmp(&b.0));
            wall.windows(2)
                .filter(|w| w[1].0 - w[0].0 > 0.01)
                .map(|w| w[0].1.dot(w[1].1).clamp(-1.0, 1.0).acos().to_degrees())
                .collect()
        };
        let (a, b) = (steps(&plain), steps(&round));
        assert!(a.len() >= 3 && b.len() >= 3, "{a:?} {b:?}");
        for (p, r) in a.iter().zip(&b) {
            assert!(*r > 0.9 * p, "rounded wall shades flat: {b:?} vs {a:?}");
        }
        assert_eq!(creased_positions(&round), 0, "no creased rings");
    }

    #[test]
    fn sweep_errors_are_clear() {
        let tri = vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]];
        let e = |r: Result<Part, ModelerError>| match r {
            Ok(_) => panic!("expected an error"),
            Err(e) => e.to_string(),
        };
        assert!(
            e(extrude(
                "x",
                &[vec![[0.0, 0.0], [1.0, 0.0]]],
                1.0,
                0.0,
                [1.0, 1.0],
                0,
                0.0
            ))
            .contains("at least 3")
        );
        assert!(
            e(extrude(
                "x",
                std::slice::from_ref(&tri),
                0.0,
                0.0,
                [1.0, 1.0],
                0,
                0.0
            ))
            .contains("height")
        );
        assert!(
            e(revolve(
                "x",
                &[vec![[-1.0, 0.0], [1.0, 0.0], [0.0, 1.0]]],
                360.0,
                0,
                0.0
            ))
            .contains("radius >= 0")
        );
        assert!(e(revolve("x", &[tri], 400.0, 0, 0.0)).contains("degrees"));
    }

    #[test]
    fn real_scale_chess_king_union_is_closed() {
        // A 10 cm king: revolve + box cross whose corners land on body
        // vertices. Manifold keeps the touching seams as separate verts;
        // validation must still see a closed solid.
        let k = 0.15f32;
        let prof: Vec<[f32; 2]> = [
            [0.0, 0.0],
            [0.2, 0.0],
            [0.2, 0.05],
            [0.15, 0.09],
            [0.1, 0.12],
            [0.07, 0.45],
            [0.12, 0.5],
            [0.07, 0.53],
            [0.09, 0.62],
            [0.0, 0.65],
        ]
        .iter()
        .map(|p| [p[0] * k, p[1] * k])
        .collect();
        let mut body = revolve("body", &[prof], 360.0, 0, 0.0).unwrap_or_else(|e| panic!("{e}"));
        body.translation = [0.01875, 0.017, -0.13125];
        let mut cv =
            crate::primitives::primitive(crate::PrimitiveKind::Box, [0.006, 0.03, 0.006], "cv")
                .unwrap_or_else(|e| panic!("{e}"));
        cv.translation = [0.01875, 0.117, -0.13125];
        let m = Model {
            parts: vec![body.into(), cv.into()],
            ..Default::default()
        };
        let out =
            boolean(&m, BoolOp::Union, "body", "cv", Some("k1")).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(crate::verify::open_edge_count(&out), 0);
        // And a 2 mm pip cut out of a 16 mm die really shrinks it.
        let mut die = crate::primitives::primitive(crate::PrimitiveKind::Box, [0.016; 3], "die")
            .unwrap_or_else(|e| panic!("{e}"));
        die.translation = [0.0, 0.008, 0.0];
        let mut pip =
            crate::primitives::primitive(crate::PrimitiveKind::Sphere, [0.0018, 0.0, 0.0], "pip")
                .unwrap_or_else(|e| panic!("{e}"));
        pip.translation = [0.0, 0.016, 0.0];
        let m = Model {
            parts: vec![die.into(), pip.into()],
            ..Default::default()
        };
        let cut =
            boolean(&m, BoolOp::Difference, "die", "pip", None).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(crate::verify::open_edge_count(&cut), 0);
        assert!(signed_volume(&cut) < 0.016f64.powi(3) - 1e-9);
    }

    fn unit_box() -> Part {
        crate::primitives::primitive(crate::PrimitiveKind::Box, [1.0; 3], "b")
            .unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn sweep_open_pipe_and_closed_ring_are_solid() {
        let circle: Vec<[f32; 2]> = (0..16)
            .map(|k| {
                let a = k as f32 / 16.0 * std::f32::consts::TAU;
                [0.1 * a.cos(), 0.1 * a.sin()]
            })
            .collect();
        // An L-shaped pipe: straight, bend, straight.
        let path = [[0.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 1.0, 0.0]];
        let p =
            sweep("pipe", &circle, &path, false, 0.0, 0.0, 1.0).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(crate::verify::open_edge_count(&p), 0);
        let v = signed_volume(&p);
        // ~ pi r^2 * 2 m of centreline.
        let want = std::f64::consts::PI * 0.01 * 2.0;
        assert!(v > 0.0 && (v - want).abs() < want * 0.1, "{v} vs {want}");
        // A square loop: a closed ring with no caps.
        let sq = [
            [0.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [2.0, 0.0, 2.0],
            [0.0, 0.0, 2.0],
        ];
        let r = sweep("loop", &circle, &sq, true, 0.0, 0.0, 1.0).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(crate::verify::open_edge_count(&r), 0);
        assert!(signed_volume(&r) > 0.0);
        // Too short a path errors clearly.
        assert!(sweep("x", &circle, &path[..1], false, 0.0, 0.0, 1.0).is_err());
    }

    #[test]
    fn fillet_and_chamfer_keep_the_outer_size() {
        let b = unit_box();
        let start = std::time::Instant::now();
        let f = round("f", &b, "fillet", 0.1, 12).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            start.elapsed().as_secs_f32() < 5.0,
            "fillet took {:?}",
            start.elapsed()
        );
        let (lo, hi) = bounds(&f);
        assert!(
            (hi - lo - Vec3::ONE).abs().max_element() < 1e-3,
            "{lo} {hi}"
        );
        let v = signed_volume(&f);
        assert!(v < 1.0 - 1e-3 && v > 0.9, "{v}");
        assert_eq!(crate::verify::open_edge_count(&f), 0);
        let c = round("c", &b, "chamfer", 0.1, 0).unwrap_or_else(|e| panic!("{e}"));
        let vc = signed_volume(&c);
        assert!(vc < v, "a chamfer removes more than a fillet: {vc} vs {v}");
        let s = round("s", &b, "smooth", 0.0, 2).unwrap_or_else(|e| panic!("{e}"));
        assert!(signed_volume(&s) < 1.0 && s.verts.len() > b.verts.len());
        assert!(
            round("x", &b, "fillet", 0.6, 0).is_err(),
            "radius beyond half the box"
        );
    }

    #[test]
    fn hull_of_two_spheres_is_a_capsule() {
        let mut a =
            crate::primitives::primitive(crate::PrimitiveKind::Sphere, [0.5, 0.0, 0.0], "a")
                .unwrap_or_else(|e| panic!("{e}"));
        let mut b = a.clone();
        a.translation = [0.0, 0.0, 0.0];
        b.translation = [0.0, 2.0, 0.0];
        let h = hull("pill", &[&a, &b]).unwrap_or_else(|e| panic!("{e}"));
        let (lo, hi) = bounds(&h);
        assert!(
            (lo.y + 0.5).abs() < 1e-3 && (hi.y - 2.5).abs() < 1e-3,
            "{lo} {hi}"
        );
        assert_eq!(crate::verify::open_edge_count(&h), 0);
    }

    #[test]
    fn rounded_profile_corners_shrink_the_area() {
        let sq = vec![[-0.5, -0.5], [0.5, -0.5], [0.5, 0.5], [-0.5, 0.5]];
        let sharp = extrude("s", std::slice::from_ref(&sq), 1.0, 0.0, [1.0, 1.0], 0, 0.0)
            .unwrap_or_else(|e| panic!("{e}"));
        let soft =
            extrude("r", &[sq], 1.0, 0.0, [1.0, 1.0], 0, 0.1).unwrap_or_else(|e| panic!("{e}"));
        let (vs, vr) = (signed_volume(&sharp), signed_volume(&soft));
        // Four corners lose (1 - pi/4) r^2 each.
        let want = 1.0 - 4.0 * (1.0 - std::f64::consts::FRAC_PI_4) * 0.01;
        assert!(
            (vs - 1.0).abs() < 1e-4 && (vr - want).abs() < 2e-3,
            "{vs} {vr} {want}"
        );
    }
}
