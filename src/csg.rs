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
    let mut props = Vec::with_capacity(src.verts.len() * N_PROPS);
    for (i, v) in src.verts.iter().enumerate() {
        let p = rot * (Vec3::from(*v) * scale) + t;
        let uv = src.uvs.get(i).copied().unwrap_or([0.0; 2]);
        props.extend([
            p.x as f64,
            p.y as f64,
            p.z as f64,
            uv[0] as f64,
            uv[1] as f64,
        ]);
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
    let mut first: std::collections::HashMap<[i64; 3], usize> = Default::default();
    let mut rep = vec![0usize; count];
    let (mut merge_from, mut merge_to) = (Vec::new(), Vec::new());
    for (i, r) in rep.iter_mut().enumerate() {
        let f = *first.entry(key(i)).or_insert(i);
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
    finish_sweep(&m, name, "revolve")
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
fn rounded_rings(profile: &[Vec<[f32; 2]>], round: f32) -> Vec<Vec<[f64; 2]>> {
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
) -> Result<Part, ModelerError> {
    check_profile(&[profile.to_vec()])?;
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
    let mut part = Part::new(name);
    for (i, p) in pts.iter().enumerate() {
        let (t, nrm, bin, miter) = frames[i];
        for q in &ring2 {
            let (u, v) = (q[0] as f32, q[1] as f32);
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
pub fn shell(name: &str, part: &Part, t: f32, open: &[String]) -> Result<Part, ModelerError> {
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
    if tris > 20_000 {
        return Err(ModelerError::Csg(format!(
            "shell `{name}`: `{}` has {tris} triangles; shell simpler inputs (lower the segment count)",
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
        let p = sweep("pipe", &circle, &path, false, 0.0).unwrap_or_else(|e| panic!("{e}"));
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
        let r = sweep("loop", &circle, &sq, true, 0.0).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(crate::verify::open_edge_count(&r), 0);
        assert!(signed_volume(&r) > 0.0);
        // Too short a path errors clearly.
        assert!(sweep("x", &circle, &path[..1], false, 0.0).is_err());
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
