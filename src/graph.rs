//! The procedural op graph: an ordered list of [`Op`] steps that derive the
//! model's parts from each other, evaluated top to bottom. An op may reference
//! earlier outputs by name; re-issuing an op with the same output name replaces
//! it (that is how parameters are "tweaked"). No game code.

use crate::{ModelerError, Op, Part, primitives, quat_of, uv};
use bevy::math::{Quat, Vec3};
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

/// Evaluates every op and returns the derived parts. Ops run in dependency
/// order (each op after the op that produces its inputs), falling back to
/// list order wherever that is already consistent — so fresh graphs behave
/// exactly as written, while upserts that left an op positioned before an
/// input it gained later still evaluate correctly.
///
/// Each op's outputs are memoized in a process-wide, content-addressed cache
/// keyed by (op parameters, the content hashes of its input parts), so tweaking
/// one op re-runs only it and what depends on it; booleans upstream of the
/// change are free. See [`evaluate_uncached`] for verification.
pub fn evaluate(ops: &[Op]) -> Result<Vec<Arc<Part>>, ModelerError> {
    evaluate_with(ops, true)
}

/// Like [`evaluate`] but ignores and never fills the cache: always a fresh
/// derivation, for `check_rebuild` (a cached result would hide
/// non-determinism and stale-cache bugs).
pub fn evaluate_uncached(ops: &[Op]) -> Result<Vec<Arc<Part>>, ModelerError> {
    evaluate_with(ops, false)
}

/// Cap on cached op results; the cache is dropped wholesale when exceeded
/// (cheap, and the working set re-fills as the graph is re-evaluated).
const CACHE_MAX_ENTRIES: usize = 2048;

/// Named parts during evaluation. Shared (`Arc`) so cache hits and the
/// returned parts reuse meshes instead of deep-copying them.
type Named = Vec<(String, Arc<Part>)>;

type OpCache = HashMap<u64, Named>;

thread_local! {
    /// Ops actually executed (cache misses) on this thread; lets tests prove
    /// what was reused.
    static RUNS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Ops executed (not served from the cache) on the calling thread so far.
pub fn ops_run_on_thread() -> u64 {
    RUNS.with(|r| r.get())
}

static OP_CACHE: Mutex<Option<OpCache>> = Mutex::new(None);

/// Number of memoized op results (for tests and diagnostics).
pub fn cache_len() -> usize {
    OP_CACHE
        .lock()
        .map(|c| c.as_ref().map_or(0, HashMap::len))
        .unwrap_or(0)
}

fn hash_of(bytes: &[u8], inputs: &[u64]) -> u64 {
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    inputs.hash(&mut h);
    h.finish()
}

fn evaluate_with(ops: &[Op], use_cache: bool) -> Result<Vec<Arc<Part>>, ModelerError> {
    let order = eval_order(ops)?;
    let mut named: Named = Vec::new();
    // Content hash of each named part: the key of the op run that wrote it,
    // combined with the output name.
    let mut hashes: HashMap<String, u64> = HashMap::new();
    for i in order {
        let op = &ops[i];
        if matches!(
            op,
            Op::Move { .. } | Op::Light { .. } | Op::Collider { .. } | Op::Marker { .. }
        ) {
            continue;
        }
        let input_hashes: Vec<u64> = inputs(op)
            .iter()
            .map(|n| hashes.get(*n).copied().unwrap_or(0))
            .collect();
        let key = hash_of(
            &serde_json::to_vec(op).map_err(|e| e.to_string())?,
            &input_hashes,
        );
        let hit = if use_cache && !matches!(op, Op::Rename { .. }) {
            OP_CACHE
                .lock()
                .ok()
                .and_then(|c| c.as_ref().and_then(|m| m.get(&key).cloned()))
        } else {
            None
        };
        let outs = outputs(op);
        match hit {
            Some(parts) => {
                for (name, part) in parts {
                    upsert_shared(&mut named, &name, part);
                }
            }
            None => {
                RUNS.with(|r| r.set(r.get() + 1));
                run_op(op, &mut named)?;
                if use_cache && !matches!(op, Op::Rename { .. }) {
                    let produced: Named = outs
                        .iter()
                        .filter_map(|n| {
                            named
                                .iter()
                                .find(|(m, _)| m == n)
                                .map(|(m, p)| (m.clone(), p.clone()))
                        })
                        .collect();
                    if let Ok(mut c) = OP_CACHE.lock() {
                        let map = c.get_or_insert_with(HashMap::new);
                        if map.len() >= CACHE_MAX_ENTRIES {
                            map.clear();
                        }
                        map.insert(key, produced);
                    }
                }
            }
        }
        if let Op::Rename { input, .. } = op {
            hashes.remove(input);
        }
        for n in outs {
            hashes.insert(n.clone(), hash_of(n.as_bytes(), &[key]));
        }
    }
    // Procedural materials map their parts last, so cuts and copies made
    // after the material op are mapped too (one tile = its real size).
    for (_, part) in named.iter_mut() {
        if part.material.pbr.is_some() {
            crate::material::pbr::map_part(Arc::make_mut(part))?;
        }
    }
    // Parent transforms, applied to the finished parts in list order,
    // except that a place runs after every move/place that moves its `on`
    // part (a pot placed on a sill that is itself mounted on a wall).
    for op in move_order(ops) {
        let Op::Move {
            name: move_name,
            parts,
            translation,
            rotation,
            pivot,
            on,
            at,
            gap,
            toward,
            each,
        } = op
        else {
            continue;
        };
        let r = rotation.map(quat_of).unwrap_or(bevy::math::Quat::IDENTITY);
        let t = Vec3::from(translation.unwrap_or([0.0; 3]));
        let p = Vec3::from(*pivot);
        for (name, part) in named.iter_mut() {
            let hit = parts.iter().any(|pat| crate::name_matches(pat, name));
            if !hit {
                continue;
            }
            let part = Arc::make_mut(part);
            part.translation = (p + r * (Vec3::from(part.translation) - p) + t).to_array();
            part.rotation = (r * quat_of(part.rotation)).to_array();
        }
        if let Some(on) = on {
            place_on(
                &mut named,
                move_name,
                parts,
                on,
                *at,
                gap.unwrap_or(0.0),
                toward.as_deref().unwrap_or("down"),
                *each,
            )?;
        }
    }
    attach_occluders(&mut named);
    Ok(named.into_iter().map(|(_, part)| part).collect())
}

/// Gives every visible part whose object-domain material reads the mesh
/// (`mesh_ao`, `mesh_curvature`) the visible solids near it, in its own
/// local space, so creases between parts darken. Runs after moves.
fn attach_occluders(named: &mut Named) {
    let reads_mesh = |p: &Part| {
        p.visible
            && p.material
                .pbr
                .as_ref()
                .and_then(|r| r.graph.as_ref())
                .is_some_and(|g| {
                    g.domain == "object"
                        && g.nodes.iter().any(|n| {
                            matches!(
                                n.get("type").and_then(|t| t.as_str()),
                                Some("mesh_ao" | "mesh_curvature")
                            )
                        })
                })
    };
    let wants: Vec<usize> = (0..named.len())
        .filter(|&i| reads_mesh(&named[i].1))
        .collect();
    if wants.is_empty() {
        return;
    }
    // Bounds first (a vertex scan, no allocation); world triangles only for
    // parts that are actually near a part that wants them.
    let boxes: Vec<Option<(Vec3, Vec3)>> = named
        .iter()
        .map(|(_, p)| (p.visible && !p.verts.is_empty()).then(|| p.bounds()))
        .collect();
    let mut tris_of: HashMap<usize, Vec<[Vec3; 3]>> = HashMap::new();
    for &i in &wants {
        let Some((lo, hi)) = boxes[i] else {
            continue;
        };
        let m = (hi - lo).max_element() * 0.25;
        let (lo, hi) = (lo - m, hi + m);
        let near: Vec<usize> = (0..named.len())
            .filter(|&j| j != i)
            .filter(|&j| boxes[j].is_some_and(|(l2, h2)| l2.cmple(hi).all() && h2.cmpge(lo).all()))
            .collect();
        let part = &named[i].1;
        let r = quat_of(part.rotation).inverse();
        let t = Vec3::from(part.translation);
        let occ: Vec<Vec<[[f32; 3]; 3]>> = near
            .iter()
            .map(|&j| {
                tris_of
                    .entry(j)
                    .or_insert_with(|| crate::contact::world_tris(&named[j].1))
                    .iter()
                    .map(|tr| tr.map(|v| (r * (v - t)).to_array()))
                    .collect()
            })
            .collect();
        Arc::make_mut(&mut named[i].1).occluders = (!occ.is_empty()).then(|| Arc::new(occ));
    }
}

/// The move/place ops in run order: written order, but each place after
/// the moves that move the part it lands on (cycles keep written order).
fn move_order(ops: &[Op]) -> Vec<&Op> {
    let moves: Vec<&Op> = ops
        .iter()
        .filter(|o| matches!(o, Op::Move { .. }))
        .collect();
    let n = moves.len();
    let deps: Vec<Vec<usize>> = (0..n)
        .map(|i| {
            let Op::Move { on: Some(on), .. } = moves[i] else {
                return Vec::new();
            };
            (0..n)
                .filter(|&j| {
                    j != i
                        && matches!(moves[j], Op::Move { parts, .. }
                            if parts.iter().any(|p| crate::name_matches(p, on)))
                })
                .collect()
        })
        .collect();
    let mut state = vec![0u8; n]; // 0 new, 1 visiting, 2 done
    let mut order = Vec::with_capacity(n);
    fn visit(i: usize, deps: &[Vec<usize>], state: &mut [u8], order: &mut Vec<usize>) {
        if state[i] != 0 {
            return;
        }
        state[i] = 1;
        for &j in &deps[i] {
            visit(j, deps, state, order);
        }
        state[i] = 2;
        order.push(i);
    }
    for i in 0..n {
        visit(i, &deps, &mut state, &mut order);
    }
    order.into_iter().map(|i| moves[i]).collect()
}

/// The `place` step of a move: slide the matched parts to `at` (footprint
/// centre, x/z), then drop (or lift) them vertically until their underside
/// rests on `on`'s top surface plus `gap`. Clearance is sampled along
/// vertical lines through every vertex of both, so a small part on a big top
/// and a big part on a small post both land.
fn place_on(
    named: &mut Named,
    move_name: &str,
    patterns: &[String],
    on: &str,
    at: Option<[f32; 2]>,
    gap: f32,
    toward: &str,
    each: bool,
) -> Result<(), ModelerError> {
    // Travel axis k with sign, and the two cross axes `at` names.
    let (k, sign, (u, w)) = match toward {
        "down" => (1, -1.0, (0, 2)),
        "up" => (1, 1.0, (0, 2)),
        "back" => (2, -1.0, (0, 1)),
        "front" => (2, 1.0, (0, 1)),
        "left" => (0, -1.0, (2, 1)),
        "right" => (0, 1.0, (2, 1)),
        other => {
            return Err(format!(
                "place `{move_name}`: toward `{other}`: use down, up, left, right, back or front"
            )
            .into());
        }
    };
    let mut d = Vec3::ZERO;
    d[k] = sign;
    let hit = |n: &str| n != on && patterns.iter().any(|p| crate::name_matches(p, n));
    let Some(support) = named.iter().find(|(n, _)| n == on).map(|(_, p)| p.clone()) else {
        return Err(format!("place `{move_name}`: no part `{on}` to place on").into());
    };
    let moving: Vec<usize> = (0..named.len()).filter(|&i| hit(&named[i].0)).collect();
    if moving.is_empty() {
        return Err(format!("place `{move_name}`: no part matches {patterns:?}").into());
    }
    let groups: Vec<Vec<usize>> = if each {
        moving.iter().map(|&i| vec![i]).collect()
    } else {
        vec![moving]
    };
    let s_tris = crate::contact::world_tris(&support);
    // Rays all run along the travel axis: bin the support's triangles across
    // it once for every group.
    let s_grid = crate::contact::AxisGrid::new(&s_tris, u, w);
    for moving in groups {
        place_group(
            named, move_name, patterns, on, at, gap, toward, &support, &s_tris, &s_grid, &moving,
            d, u, w,
        )?;
    }
    Ok(())
}

/// One rigid group of [`place_on`]: slide to `at`, then travel along `d`
/// until it meets `support`.
#[allow(clippy::too_many_arguments)]
fn place_group(
    named: &mut Named,
    move_name: &str,
    patterns: &[String],
    on: &str,
    at: Option<[f32; 2]>,
    gap: f32,
    toward: &str,
    support: &Part,
    s_tris: &[[Vec3; 3]],
    s_grid: &crate::contact::AxisGrid<'_>,
    moving: &[usize],
    d: Vec3,
    u: usize,
    w: usize,
) -> Result<(), ModelerError> {
    let bounds = |named: &Named| {
        moving.iter().fold((Vec3::MAX, Vec3::MIN), |(lo, hi), &i| {
            let (a, b) = named[i].1.bounds();
            (lo.min(a), hi.max(b))
        })
    };
    if let Some([a0, a1]) = at {
        let (lo, hi) = bounds(named);
        let mut shift = Vec3::ZERO;
        shift[u] = a0 - (lo[u] + hi[u]) * 0.5;
        shift[w] = a1 - (lo[w] + hi[w]) * 0.5;
        for &i in moving {
            let p = Arc::make_mut(&mut named[i].1);
            p.translation = (Vec3::from(p.translation) + shift).to_array();
        }
    }
    let m_tris: Vec<[Vec3; 3]> = moving
        .iter()
        .flat_map(|&i| crate::contact::world_tris(&named[i].1))
        .collect();
    // Heights are measured against the travel: h = -d . p (for "down", y).
    let (slo, shi) = support.bounds();
    let (mlo, mhi) = bounds(named);
    let h = |v: Vec3| -v.dot(d);
    let corners = [slo, shi, mlo, mhi];
    let top = corners.iter().map(|&v| h(v)).fold(f32::MIN, f32::max) + 1.0;
    let bottom = corners.iter().map(|&v| h(v)).fold(f32::MAX, f32::min) - 1.0;
    let at_h = |v: Vec3, hh: f32| v + d * (h(v) - hh);
    // The support's surface facing the parts, from their side; the parts'
    // leading surface, from the support's side.
    let m_grid = crate::contact::AxisGrid::new(&m_tris, u, w);
    let surface = |v: Vec3| s_grid.cast(at_h(v, top), d).map(|t| top - t);
    let leading = |v: Vec3| m_grid.cast(at_h(v, bottom), -d).map(|t| bottom + t);
    let mut clearance = f32::MAX;
    let samples = s_tris.iter().chain(&m_tris).flat_map(|t| {
        let c = (t[0] + t[1] + t[2]) / 3.0;
        [t[0], t[1], t[2], c]
    });
    for v in samples {
        if let (Some(lead), Some(surf)) = (leading(v), surface(v)) {
            clearance = clearance.min(lead - surf);
        }
    }
    if clearance == f32::MAX {
        let axes = ["x", "y", "z"];
        return Err(format!(
            "place `{move_name}`: nothing of {patterns:?} lines up with `{on}` travelling {toward} (give at: [{}, {}] facing it)",
            axes[u], axes[w]
        )
        .into());
    }
    let shift = d * (clearance - gap);
    for &i in moving {
        let p = Arc::make_mut(&mut named[i].1);
        p.translation = (Vec3::from(p.translation) + shift).to_array();
    }
    Ok(())
}

/// Computes a run order for `ops`. Each op depends on the nearest earlier op
/// producing each of its inputs (matching lookup semantics); when no earlier
/// producer exists, it depends on the nearest producer anywhere, healing
/// graphs where an upsert left an op before an input it now reads. Ties are
/// broken by list index, so already-consistent graphs keep their written
/// order. Remaining ops after the loop form a dependency cycle.
fn eval_order(ops: &[Op]) -> Result<Vec<usize>, ModelerError> {
    let n = ops.len();
    let mut deps: Vec<Vec<usize>> = vec![Vec::new(); n];
    // Indexes built once: part name -> ops producing it (ascending), and
    // part name -> ops reading it, so dependency lookup is not O(n) per input.
    let mut producers: HashMap<String, Vec<usize>> = HashMap::new();
    let mut readers: HashMap<&str, Vec<usize>> = HashMap::new();
    for (j, op) in ops.iter().enumerate() {
        for name in outputs(op) {
            producers.entry(name).or_default().push(j);
        }
        for input in inputs(op) {
            let list = readers.entry(input).or_default();
            if list.last() != Some(&j) {
                list.push(j);
            }
        }
    }
    for (i, op) in ops.iter().enumerate() {
        for input in inputs(op) {
            let Some(list) = producers.get(input) else {
                continue;
            };
            // Nearest earlier producer, else the nearest later one.
            let split = list.partition_point(|&j| j < i);
            let dep = if split > 0 {
                Some(list[split - 1])
            } else {
                list.iter().copied().find(|&j| j > i)
            };
            if let Some(j) = dep {
                deps[i].push(j);
            }
        }
    }
    // Material lineage: an in-place material op on X runs before every other
    // op that reads X (arrays, mirrors, transforms, booleans), so copies inherit
    // the color wherever the material op sits in the list. Readers that also
    // write X (other in-place edits) keep list order to avoid cycles.
    for (m, op) in ops.iter().enumerate() {
        if let Op::Material { name, input, .. } = op
            && name == input
        {
            for &j in readers.get(input.as_str()).into_iter().flatten() {
                if j != m
                    && !outputs(&ops[j]).iter().any(|o| o == input)
                    && !matches!(ops[j], Op::Rename { .. })
                {
                    deps[j].push(m);
                }
            }
        }
    }
    // A rename removes the old name, so it must run after every other op
    // that reads that name.
    for (i, op) in ops.iter().enumerate() {
        if let Op::Rename { input, .. } = op {
            for &j in readers.get(input.as_str()).into_iter().flatten() {
                if j != i {
                    deps[i].push(j);
                }
            }
        }
    }
    let mut order = Vec::with_capacity(n);
    let mut done = vec![false; n];
    let mut remaining = n;
    while remaining > 0 {
        let mut progressed = false;
        for i in 0..n {
            if !done[i] && deps[i].iter().all(|&d| done[d]) {
                done[i] = true;
                order.push(i);
                remaining -= 1;
                progressed = true;
            }
        }
        if !progressed {
            let stuck: Vec<String> = (0..n)
                .filter(|&i| !done[i])
                .map(|i| format!("`{}`", output(&ops[i])))
                .collect();
            return Err(ModelerError::Cycle(stuck.join(", ")));
        }
    }
    Ok(order)
}

/// Runs one op against the named-part map, upserting its output.
fn run_op(op: &Op, named: &mut Named) -> Result<(), ModelerError> {
    match op {
        // Applied after the graph, in `evaluate_with`.
        Op::Move { .. } | Op::Light { .. } | Op::Collider { .. } | Op::Marker { .. } => {}
        Op::Primitive {
            name,
            kind,
            size,
            translation,
            rotation,
            frame,
        } => {
            let mut part =
                primitives::primitive(*kind, *size, name).map_err(|e| format!("{name}: {e}"))?;
            part.translation = *translation;
            if let Some(r) = rotation {
                part.rotation = *r;
            }
            if let Some(frame) = frame {
                let (t, r) = frame_of(named, name, frame)?;
                part.translation = (t + r * Vec3::from(part.translation)).to_array();
                part.rotation = (r * quat_of(part.rotation)).to_array();
            }
            upsert(named, name, part);
        }
        Op::Extrude {
            name,
            profile,
            height,
            twist,
            scale_top,
            slices,
            round,
            translation,
            rotation,
        } => {
            let mut part =
                crate::csg::extrude(name, profile, *height, *twist, *scale_top, *slices, *round)?;
            part.translation = *translation;
            if let Some(r) = rotation {
                part.rotation = *r;
            }
            upsert(named, name, part);
        }
        Op::Revolve {
            name,
            profile,
            degrees,
            segments,
            round,
            translation,
            rotation,
        } => {
            let mut part = crate::csg::revolve(name, profile, *degrees, *segments, *round)?;
            part.translation = *translation;
            if let Some(r) = rotation {
                part.rotation = *r;
            }
            upsert(named, name, part);
        }
        Op::Sweep {
            name,
            profile,
            path,
            closed,
            round,
        } => {
            let part = crate::csg::sweep(name, profile, path, *closed, *round)?;
            upsert(named, name, part);
        }
        Op::Hull { name, inputs } => {
            let parts: Vec<Arc<Part>> = inputs
                .iter()
                .map(|i| lookup_shared(named, i))
                .collect::<Result<_, _>>()?;
            let refs: Vec<&Part> = parts.iter().map(|p| p.as_ref()).collect();
            let part = crate::csg::hull(name, &refs)?;
            upsert(named, name, part);
        }
        Op::Round {
            name,
            input,
            mode,
            radius,
            segments,
        } => {
            let src = lookup_shared(named, input)?;
            let part = crate::csg::round(name, &src, mode, *radius, *segments)?;
            upsert(named, name, part);
        }
        Op::Shell {
            name,
            input,
            thickness,
            open,
        } => {
            let src = lookup_shared(named, input)?;
            let part = crate::csg::shell(name, &src, *thickness, open)?;
            upsert(named, name, part);
        }
        Op::Group { name, inputs } => {
            if inputs.is_empty() {
                return Err(ModelerError::Invalid(format!(
                    "group `{name}` has no inputs"
                )));
            }
            let mut out = Part::new(name);
            for (k, input) in inputs.iter().enumerate() {
                let p = lookup_shared(named, input)?;
                if k == 0 {
                    out.material = p.material.clone();
                }
                append_baked(&mut out, &p);
            }
            upsert(named, name, out);
        }
        Op::Transform {
            name,
            input,
            translation,
            rotation,
            scale,
            relative,
            pivot,
        } => {
            let mut part = lookup(named, input)?;
            part.name = name.clone();
            if name != input {
                part.visible = true;
            }
            if *relative {
                // Offsets from the input: translation adds, rotation composes
                // on top, scale multiplies.
                // About a pivot: the part's origin swings around it too.
                if let Some(p) = pivot {
                    let p = Vec3::from(*p);
                    let mut off = Vec3::from(part.translation) - p;
                    if let Some(s) = scale {
                        off *= Vec3::from(*s);
                    }
                    if let Some(r) = rotation {
                        off = quat_of(*r) * off;
                    }
                    part.translation = (p + off).to_array();
                }
                if let Some(t) = translation {
                    part.translation = (Vec3::from(part.translation) + Vec3::from(*t)).to_array();
                }
                if let Some(r) = rotation {
                    part.rotation = (quat_of(*r) * quat_of(part.rotation)).to_array();
                }
                if let Some(s) = scale {
                    part.scale = (Vec3::from(part.scale) * Vec3::from(*s)).to_array();
                }
            } else {
                if let Some(t) = translation {
                    part.translation = *t;
                }
                if let Some(r) = rotation {
                    part.rotation = *r;
                }
                if let Some(s) = scale {
                    part.scale = *s;
                }
            }
            upsert(named, name, part);
        }
        Op::Boolean { name, op, a, b } => {
            // `csg::boolean` looks parts up on a model and bakes their
            // transforms into the verts, so hand it a scratch pair.
            let mut scratch = crate::Model::named("scratch");
            scratch.parts = vec![lookup_shared(named, a)?, lookup_shared(named, b)?];
            let part = crate::csg::boolean(&scratch, *op, a, b, Some(name))?;
            upsert(named, name, part);
        }
        Op::RadialArray {
            name,
            input,
            count,
            center,
            degrees,
            start_degrees,
            axis,
            include_source,
        } => {
            let mut source = lookup(named, input)?;
            if *count == 0 {
                return Err(format!("{name}: count must be at least 1").into());
            }
            // Copies inherit the source's UVs; project once only if it has none.
            if source.uvs.len() != source.verts.len() {
                uv::ensure_uvs(&mut source, Some("box")).map_err(|e| format!("{name}: {e}"))?;
            }
            let axis_vec = axis.unit();
            let step = degrees / *count as f32;
            // With the source as copy 0, only slots 1..count are new.
            for i in u32::from(*include_source)..*count {
                let yaw = start_degrees + i as f32 * step;
                let mut part = source.clone();
                part.name = format!("{name}_{i}");
                part.visible = true;
                let rot = Quat::from_axis_angle(axis_vec, yaw.to_radians());
                let offset = Vec3::from(part.translation) - Vec3::from(*center);
                part.translation = (rot * offset + Vec3::from(*center)).to_array();
                part.rotation = (rot * quat_of(part.rotation)).to_array();
                upsert(named, &format!("{name}_{i}"), part);
            }
        }
        Op::Material {
            name,
            input,
            base_color,
            roughness,
            metalness,
            pbr,
            flat,
        } => {
            let mut part = lookup(named, input)?;
            part.name = name.clone();
            if name != input {
                part.visible = true;
            }
            if let Some(r) = pbr {
                r.graph()?;
                // Factors multiply the textures: white / 1 unless given.
                part.material.base_color = [1.0; 4];
                part.material.roughness = 1.0;
                part.material.metalness = 1.0;
                part.material.pbr = Some(r.clone());
            }
            if let Some(c) = base_color {
                part.material.base_color = *c;
            }
            if let Some(r) = roughness {
                part.material.roughness = *r;
            }
            if let Some(m) = metalness {
                part.material.metalness = *m;
            }
            if *flat {
                part.material.flat = true;
            } else if pbr.is_some() {
                part.material.flat = false;
            }
            upsert(named, name, part);
        }
        Op::LinearArray {
            name,
            input,
            count,
            offset,
            local,
        } => {
            let source = lookup(named, input)?;
            if *count == 0 {
                return Err(format!("{name}: count must be at least 1").into());
            }
            let offset = if *local {
                (quat_of(source.rotation) * Vec3::from(*offset)).to_array()
            } else {
                *offset
            };
            // The source is copy 0; copies 1..count step away from it,
            // so nothing overlaps the input.
            for i in 1..*count {
                let mut part = source.clone();
                part.name = format!("{name}_{i}");
                part.visible = true;
                let i = i as f32;
                part.translation = [
                    part.translation[0] + offset[0] * i,
                    part.translation[1] + offset[1] * i,
                    part.translation[2] + offset[2] * i,
                ];
                let part_name = part.name.clone();
                upsert(named, &part_name, part);
            }
        }
        Op::Mirror {
            name,
            input,
            axis,
            offset,
        } => {
            let ax = axis.index();
            let source = lookup(named, input)?;
            let plane = offset[ax];
            let mut part = source.clone();
            part.name = name.clone();
            part.visible = name != input || part.visible;
            // Bake: world verts, reflect across the plane, flip winding.
            let rot = quat_of(part.rotation);
            let scale = Vec3::from(part.scale);
            let translation = Vec3::from(part.translation);
            let mut tris = Vec::with_capacity(part.tri_count() as usize);
            for t in part.indices.as_chunks::<3>().0 {
                let world: Vec<Vec3> = t
                    .iter()
                    .map(|&i| {
                        let mut v =
                            rot * (Vec3::from(part.verts[i as usize]) * scale) + translation;
                        v[ax] = 2.0 * plane - v[ax];
                        v
                    })
                    .collect();
                tris.push([world[0], world[1], world[2]]);
            }
            let mut verts = Vec::with_capacity(tris.len() * 3);
            let mut indices = Vec::with_capacity(tris.len() * 3);
            for (i, t) in tris.iter().enumerate() {
                for v in t {
                    verts.push(v.to_array());
                }
                // Mirrored faces wind the other way.
                let b = (i * 3) as u32;
                indices.extend([b, b + 2, b + 1]);
            }
            part.verts = verts;
            part.indices = indices;
            part.normals.clear();
            crate::uv::ensure_normals(&mut part);
            crate::uv::ensure_uvs(&mut part, Some("box"))?;
            part.translation = [0.0; 3];
            part.rotation = Quat::IDENTITY.to_array();
            part.scale = [1.0; 3];
            upsert(named, name, part);
        }
        Op::Visibility {
            name,
            input,
            visible,
        } => {
            let mut part = lookup(named, input)?;
            part.name = name.clone();
            part.visible = *visible;
            upsert(named, name, part);
        }
        Op::Prefab {
            name,
            prefab,
            translation,
            rotation,
            scale,
            parts,
            restyle,
            ..
        } => {
            let mut model = load_prefab_guarded(prefab)?;
            if let Some(st) = restyle {
                let mut s = crate::commands::Session::default();
                s.model = model;
                s.set_style(st.clone())
                    .map_err(|e| format!("{name}: restyling prefab `{prefab}`: {e}"))?;
                model = s.model;
            }
            let (it, ir) = (
                Vec3::from(*translation),
                quat_of(rotation.unwrap_or([0.0, 0.0, 0.0, 1.0])),
            );
            let is = Vec3::from(scale.unwrap_or([1.0; 3]));
            for p in parts {
                let Some(src) = model.part(p) else {
                    return Err(format!(
                        "{name}: prefab `{prefab}` no longer has part `{p}`; re-issue the prefab op"
                    )
                    .into());
                };
                let mut part = (*src).clone();
                let out = format!("{name}.{p}");
                part.name = out.clone();
                part.visible = true;
                part.translation = (it + ir * (is * Vec3::from(part.translation))).to_array();
                part.rotation = (ir * quat_of(part.rotation)).to_array();
                part.scale = (is * Vec3::from(part.scale)).to_array();
                upsert(named, &out, part);
            }
        }
        Op::Rename { name, input } => {
            let mut part = lookup(named, input)?;
            part.name = name.clone();
            // Take the old name out (keeping its slot) before inserting.
            if let Some(slot) = named.iter().position(|(n, _)| n == input) {
                named.remove(slot);
                named.insert(slot.min(named.len()), (name.clone(), Arc::new(part)));
            } else {
                upsert(named, name, part);
            }
        }
    }
    Ok(())
}

thread_local! {
    static PREFAB_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Loads a catalog prefab for instancing, refusing runaway nesting (a prefab
/// that instances itself, directly or not).
fn load_prefab_guarded(name: &str) -> Result<crate::Model, ModelerError> {
    let depth = PREFAB_DEPTH.with(|d| d.get());
    if depth >= 8 {
        return Err(
            format!("prefab `{name}`: nested more than 8 deep (does it instance itself?)").into(),
        );
    }
    PREFAB_DEPTH.with(|d| d.set(depth + 1));
    let loaded = crate::catalog::load_prefab(name);
    PREFAB_DEPTH.with(|d| d.set(depth));
    loaded
}

/// An owned copy of a named part, for ops that modify their input.
fn lookup(named: &Named, name: &str) -> Result<Part, ModelerError> {
    lookup_shared(named, name).map(|p| (*p).clone())
}

/// A shared handle to a named part, for ops that only read it.
fn lookup_shared(named: &Named, name: &str) -> Result<Arc<Part>, ModelerError> {
    named
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, p)| p.clone())
        .ok_or_else(|| ModelerError::MissingInput(name.to_string()))
}

/// Inserts `part` under `name`, replacing an earlier op's output of the same
/// name in place (so output order stays stable across re-issues).
/// Appends `p`'s mesh to `out` with `p`'s transform baked into positions and
/// normals (inverse-transpose for non-uniform scale).
fn append_baked(out: &mut Part, p: &Part) {
    let rot = crate::quat_of(p.rotation);
    let scale = crate::Vec3::from(p.scale);
    let t = crate::Vec3::from(p.translation);
    let base = out.verts.len() as u32;
    let n = p.verts.len();
    out.verts.extend(
        p.verts
            .iter()
            .map(|v| (rot * (crate::Vec3::from(*v) * scale) + t).to_array()),
    );
    let normals_ok = p.normals.len() == n && (out.normals.len() == base as usize);
    if normals_ok {
        out.normals.extend(p.normals.iter().map(|v| {
            (rot * (crate::Vec3::from(*v) / scale))
                .normalize_or_zero()
                .to_array()
        }));
    } else {
        out.normals.clear();
    }
    if p.uvs.len() == n && out.uvs.len() == base as usize {
        out.uvs.extend_from_slice(&p.uvs);
    } else {
        out.uvs.clear();
    }
    if p.indices.is_empty() {
        out.indices.extend(base..base + (n - n % 3) as u32);
    } else {
        out.indices.extend(p.indices.iter().map(|i| i + base));
    }
}

fn upsert(named: &mut Named, name: &str, part: Part) {
    upsert_shared(named, name, Arc::new(part));
}

fn upsert_shared(named: &mut Named, name: &str, part: Arc<Part>) {
    match named.iter_mut().find(|(n, _)| n == name) {
        Some(slot) => slot.1 = part,
        None => named.push((name.to_string(), part)),
    }
}

/// The output part name an op writes, used for upsert matching.
pub fn output(op: &Op) -> &str {
    match op {
        Op::Move { name, .. }
        | Op::Light { name, .. }
        | Op::Collider { name, .. }
        | Op::Marker { name, .. } => name,
        Op::Primitive { name, .. }
        | Op::Extrude { name, .. }
        | Op::Revolve { name, .. }
        | Op::Group { name, .. }
        | Op::Sweep { name, .. }
        | Op::Hull { name, .. }
        | Op::Round { name, .. }
        | Op::Shell { name, .. }
        | Op::Transform { name, .. }
        | Op::Boolean { name, .. }
        | Op::RadialArray { name, .. }
        | Op::Material { name, .. }
        | Op::LinearArray { name, .. }
        | Op::Mirror { name, .. }
        | Op::Visibility { name, .. }
        | Op::Rename { name, .. }
        | Op::Prefab { name, .. } => name,
    }
}

/// Every part name an op writes: the array ops expand to `name_0..` (radial)
/// or `name_1..` (linear, the source being copy 0); everything else writes
/// just `name`. [`output`] stays the op's identity key for upsert matching.
pub fn outputs(op: &Op) -> Vec<String> {
    match op {
        // Writes no part of its own: it re-places finished parts.
        Op::Move { .. } | Op::Light { .. } | Op::Collider { .. } | Op::Marker { .. } => Vec::new(),
        Op::RadialArray {
            name,
            count,
            include_source,
            ..
        } => {
            let first = u32::from(*include_source);
            (first..*count).map(|i| format!("{name}_{i}")).collect()
        }
        Op::LinearArray { name, count, .. } => (1..*count).map(|i| format!("{name}_{i}")).collect(),
        Op::Prefab { name, parts, .. } => parts.iter().map(|p| format!("{name}.{p}")).collect(),
        other => vec![output(other).to_string()],
    }
}

/// What kind of op this is; upserts only replace ops of the same kind, so an
/// in-place op (output name == an input name) never eats its own source.
pub fn kind(op: &Op) -> &'static str {
    match op {
        Op::Move { .. } => "move",
        Op::Light { .. } => "light",
        Op::Collider { .. } => "collider",
        Op::Marker { .. } => "marker",
        Op::Primitive { .. } => "primitive",
        Op::Extrude { .. } => "extrude",
        Op::Revolve { .. } => "revolve",
        Op::Group { .. } => "group",
        Op::Sweep { .. } => "sweep",
        Op::Hull { .. } => "hull",
        Op::Round { .. } => "round",
        Op::Shell { .. } => "shell",
        Op::Transform { .. } => "transform",
        Op::Boolean { .. } => "boolean",
        Op::RadialArray { .. } => "radial",
        Op::Material { .. } => "material",
        Op::LinearArray { .. } => "linear",
        Op::Mirror { .. } => "mirror",
        Op::Visibility { .. } => "visibility",
        Op::Rename { .. } => "rename",
        Op::Prefab { .. } => "prefab",
    }
}

/// The part names an op reads.
pub fn inputs(op: &Op) -> Vec<&str> {
    match op {
        Op::Move { .. }
        | Op::Light { .. }
        | Op::Collider { .. }
        | Op::Marker { .. }
        | Op::Primitive { frame: None, .. }
        | Op::Extrude { .. }
        | Op::Revolve { .. }
        | Op::Sweep { .. }
        | Op::Prefab { .. } => Vec::new(),
        Op::Transform { input, .. }
        | Op::RadialArray { input, .. }
        | Op::Material { input, .. }
        | Op::LinearArray { input, .. }
        | Op::Mirror { input, .. }
        | Op::Visibility { input, .. }
        | Op::Rename { input, .. } => {
            vec![input]
        }
        Op::Boolean { a, b, .. } => vec![a, b],
        Op::Group { inputs, .. } | Op::Hull { inputs, .. } => {
            inputs.iter().map(String::as_str).collect()
        }
        Op::Round { input, .. } => vec![input],
        Op::Shell { input, .. } => vec![input],
        Op::Primitive {
            frame: Some(frame), ..
        } => vec![frame_part(frame)],
    }
}

/// The part a `frame` names (`"paper"` of `"paper:top-front"`).
pub fn frame_part(frame: &str) -> &str {
    frame.split_once(':').map_or(frame, |(p, _)| p)
}

/// The world origin and rotation of a `frame`: the part's transform, with
/// the origin moved to the anchor on its local (unscaled-mesh) box.
fn frame_of(
    named: &Named,
    name: &str,
    frame: &str,
) -> Result<(Vec3, bevy::math::Quat), ModelerError> {
    let base = lookup(named, frame_part(frame)).map_err(|e| format!("{name}: frame: {e}"))?;
    let r = quat_of(base.rotation);
    let mut origin = Vec3::ZERO;
    if let Some((_, words)) = frame.split_once(':') {
        let (lo, hi) = base
            .verts
            .iter()
            .fold((Vec3::MAX, Vec3::MIN), |(lo, hi), v| {
                (lo.min(Vec3::from(*v)), hi.max(Vec3::from(*v)))
            });
        if lo.x > hi.x {
            return Err(format!("{name}: frame `{frame}` has no geometry").into());
        }
        let a = crate::commands::anchor_point(words, lo, hi)
            .map_err(|e| format!("{name}: frame: {e}"))?;
        origin = Vec3::from(a) * Vec3::from(base.scale);
    }
    Ok((Vec3::from(base.translation) + r * origin, r))
}

/// One line per op with its full parameters, for summaries and the `ops`
/// tool — so an agent can tweak an op without remembering what it sent.
pub fn describe(ops: &[Op]) -> String {
    if ops.is_empty() {
        return "ops: none (baked parts)".into();
    }
    let mut lines = vec![format!("ops ({})", ops.len())];
    for (i, op) in ops.iter().enumerate() {
        lines.push(match op {
            Op::Collider {
                name,
                parts,
                shape,
                body,
                sensor,
            } => format!(
                "#{i} collider `{name}` parts={parts:?} {shape} {body}{}",
                if *sensor { " sensor" } else { "" }
            ),
            Op::Marker {
                name,
                kind,
                translation,
                yaw,
                size,
                attach,
                ..
            } => {
                let mut s = format!("#{i} marker `{name}` ({kind}) at={translation:?}");
                if let Some(y) = yaw {
                    s += &format!(" yaw={y}");
                }
                if let Some(z) = size {
                    s += &format!(" size={z:?}");
                }
                if let Some(a) = attach {
                    s += &format!(" attach=`{a}`");
                }
                s
            }
            Op::Light {
                name,
                kind,
                color,
                intensity,
                translation,
                direction,
                attach,
                ..
            } => {
                let mut s = format!("#{i} light `{name}` ({kind}) at={translation:?} color={color:?}");
                if let Some(v) = intensity {
                    s += &format!(" intensity={v}");
                }
                if let Some(d) = direction {
                    s += &format!(" dir={d:?}");
                }
                if let Some(a) = attach {
                    s += &format!(" attach=`{a}`");
                }
                s
            }
            Op::Move {
                name,
                parts,
                translation,
                rotation,
                pivot,
                on,
                at,
                gap,
                toward,
                each,
            } => {
                let mut s = match on {
                    Some(on) => format!("#{i} place `{name}` parts={parts:?} on=`{on}`"),
                    None => format!("#{i} move `{name}` parts={parts:?} pivot={pivot:?}"),
                };
                if let Some(a) = at {
                    s += &format!(" at={a:?}");
                }
                if let Some(g) = gap {
                    s += &format!(" gap={g}");
                }
                if let Some(t) = toward {
                    s += &format!(" toward={t}");
                }
                if *each {
                    s += " each";
                }
                if let Some(r) = rotation {
                    s += &format!(" rot={r:?}");
                }
                if let Some(t) = translation {
                    s += &format!(" t={t:?}");
                }
                s
            }
            Op::Primitive {
                name,
                kind,
                size,
                translation,
                rotation,
                frame,
            } => format!(
                "#{i} primitive `{name}` ({kind}) size={size:?} at={translation:?}{}{}",
                rotation.map_or_else(String::new, |r| format!(" rot={r:?}")),
                frame
                    .as_ref()
                    .map_or_else(String::new, |f| format!(" frame=`{f}`"))
            ),
            Op::Extrude {
                name,
                profile,
                height,
                twist,
                scale_top,
                slices,
                round,
                translation,
                rotation,
            } => format!(
                "#{i} extrude `{name}` profile={profile:?} height={height} twist={twist} scale_top={scale_top:?} slices={slices} round={round} at={translation:?}{}",
                rotation.map_or_else(String::new, |r| format!(" rot={r:?}"))
            ),
            Op::Revolve {
                name,
                profile,
                degrees,
                segments,
                round,
                translation,
                rotation,
            } => format!(
                "#{i} revolve `{name}` profile={profile:?} degrees={degrees} segments={segments} round={round} at={translation:?}{}",
                rotation.map_or_else(String::new, |r| format!(" rot={r:?}"))
            ),
            Op::Sweep {
                name,
                profile,
                path,
                closed,
                round,
            } => format!(
                "#{i} sweep `{name}` profile={profile:?} path={path:?} closed={closed} round={round}"
            ),
            Op::Hull { name, inputs } => format!("#{i} hull `{name}` of {inputs:?}"),
            Op::Round {
                name,
                input,
                mode,
                radius,
                segments,
            } => format!(
                "#{i} round `{name}` <- `{input}` {mode} radius={radius} segments={segments}"
            ),
            Op::Shell {
                name,
                input,
                thickness,
                open,
            } => format!("#{i} shell `{name}` <- `{input}` thickness={thickness} open={open:?}"),
            Op::Group { name, inputs } => format!(
                "#{i} group `{name}` = {}",
                inputs
                    .iter()
                    .map(|n| format!("`{n}`"))
                    .collect::<Vec<_>>()
                    .join(" + ")
            ),
            Op::Transform {
                name,
                input,
                translation,
                rotation,
                scale,
                relative,
                pivot,
            } => {
                let mut s = format!("#{i} transform `{name}` <- `{input}`");
                if let Some(p) = pivot {
                    s += &format!(" pivot={p:?}");
                }
                if let Some(t) = translation {
                    s += &format!(" t={t:?}");
                }
                if let Some(r) = rotation {
                    s += &format!(" rot={r:?}");
                }
                if let Some(sc) = scale {
                    s += &format!(" scale={sc:?}");
                }
                if *relative {
                    s += " (relative)";
                }
                s
            }
            Op::Boolean { name, op, a, b } => {
                format!("#{i} boolean `{name}` = {op}(`{a}`, `{b}`)")
            }
            Op::RadialArray {
                name,
                input,
                count,
                center,
                degrees,
                start_degrees,
                axis,
                include_source,
            } => format!(
                "#{i} radial `{name}` = {count} x `{input}` over {degrees}° from {start_degrees}° about {axis} center={center:?}{}",
                if *include_source { " (source is copy 0)" } else { "" }
            ),
            Op::Material {
                name,
                input,
                base_color,
                roughness,
                metalness,
                pbr,
                flat,
            } => {
                let mut s = format!("#{i} material `{name}` <- `{input}`");
                if *flat {
                    s += " flat";
                }
                if let Some(c) = base_color {
                    s += &format!(" color={c:?}");
                }
                if let Some(r) = roughness {
                    s += &format!(" roughness={r}");
                }
                if let Some(m) = metalness {
                    s += &format!(" metalness={m}");
                }
                if let Some(r) = pbr {
                    s += &format!(" pbr={}", r.material);
                    if (r.scale - 1.0).abs() > 1e-6 {
                        s += &format!(" scale={}", r.scale);
                    }
                    if !r.params.is_empty() {
                        s += &format!(" params={:?}", r.params);
                    }
                }
                s
            }
            Op::LinearArray {
                name,
                input,
                count,
                offset,
                local,
            } => format!(
                "#{i} linear `{name}` = {count} x `{input}` stepping {offset:?}{}",
                if *local { " (local)" } else { "" }
            ),
            Op::Mirror {
                name,
                input,
                axis,
                offset,
            } => format!(
                "#{i} mirror `{name}` = `{input}` across {} = {}",
                axis,
                offset[axis.index()]
            ),
            Op::Visibility {
                name,
                input,
                visible,
            } => format!(
                "#{i} visibility `{name}` = `{input}` {}",
                if *visible { "shown" } else { "hidden" }
            ),
            Op::Rename { name, input } => format!("#{i} rename `{input}` -> `{name}`"),
            Op::Prefab {
                name,
                prefab,
                translation,
                rotation,
                scale,
                parts,
                ..
            } => {
                let mut s = format!(
                    "#{i} prefab `{name}` = catalog `{prefab}` at={translation:?} ({} parts: {name}.{})",
                    parts.len(),
                    parts.join(&format!(", {name}."))
                );
                if let Some(r) = rotation {
                    s += &format!(" rot={r:?}");
                }
                if let Some(sc) = scale {
                    s += &format!(" scale={sc:?}");
                }
                s
            }
        });
    }
    lines.join("\n")
}

/// The full describe line for a single op, looked up by output part name (the
/// newest op wins, matching upsert semantics). For the `op_get` tool.
pub fn describe_one(ops: &[Op], name: &str) -> Option<String> {
    // Every op writing `name`, in graph order: the producing op first, then
    // in-place edits (material, visibility, transform) that share its name.
    let lines: Vec<String> = ops
        .iter()
        .enumerate()
        .filter(|(_, op)| output(op) == name)
        .map(|(i, op)| {
            let d = describe(std::slice::from_ref(op));
            d.lines()
                .nth(1)
                .map(|l| l.replacen("#0 ", &format!("#{i} "), 1))
                .unwrap_or(d)
        })
        .collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitive_then_transform_moves_the_part() {
        let parts = evaluate(&[
            Op::Primitive {
                name: "box".into(),
                kind: crate::PrimitiveKind::Box,
                size: [1.0, 1.0, 1.0],
                translation: [0.0, 0.0, 0.0],
                rotation: None,
                frame: None,
            },
            Op::Transform {
                name: "lifted".into(),
                input: "box".into(),
                translation: Some([0.0, 2.0, 0.0]),
                rotation: None,
                scale: None,
                relative: false,
                pivot: None,
            },
        ])
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[1].name, "lifted");
        assert_eq!(parts[1].translation, [0.0, 2.0, 0.0]);
        assert_eq!(parts[0].translation, [0.0, 0.0, 0.0]);
    }

    #[test]
    fn eval_order_heals_upsert_positions() {
        // From a real session: a copy op named before its input (an upsert had
        // left the input later in the list). Dependency order must run the
        // producer first regardless of list position.
        let parts = evaluate(&[
            Op::RadialArray {
                name: "leg".into(),
                input: "leg_s".into(),
                count: 2,
                center: [0.0, 0.0, 0.0],
                degrees: 360.0,
                start_degrees: 0.0,
                axis: crate::Axis::Y,
                include_source: false,
            },
            Op::Transform {
                name: "leg_s".into(),
                input: "leg_template".into(),
                translation: Some([0.4, 0.375, 0.0]),
                rotation: None,
                scale: None,
                relative: false,
                pivot: None,
            },
            Op::Primitive {
                name: "leg_template".into(),
                kind: crate::PrimitiveKind::Box,
                size: [0.08, 0.75, 0.08],
                translation: [0.0, 0.0, 0.0],
                rotation: None,
                frame: None,
            },
        ])
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(parts.len(), 4, "template + leg_s + 2 radial copies");
        let leg_s = parts
            .iter()
            .find(|p| p.name == "leg_s")
            .unwrap_or_else(|| panic!("leg_s missing"));
        assert_eq!(leg_s.translation, [0.4, 0.375, 0.0]);
    }

    #[test]
    fn dependency_cycle_errors_and_names_the_ops() {
        let Err(err) = evaluate(&[
            Op::Transform {
                name: "a".into(),
                input: "b".into(),
                translation: None,
                rotation: None,
                scale: None,
                relative: false,
                pivot: None,
            },
            Op::Transform {
                name: "b".into(),
                input: "a".into(),
                translation: None,
                rotation: None,
                scale: None,
                relative: false,
                pivot: None,
            },
        ]) else {
            panic!("a true cycle must fail")
        };
        assert!(err.to_string().contains("dependency cycle"), "{err}");
        assert!(
            err.to_string().contains("`a`") && err.to_string().contains("`b`"),
            "{err}"
        );
    }

    #[test]
    fn reissued_op_replaces_its_output_in_place() {
        let ops = [
            Op::Primitive {
                name: "a".into(),
                kind: crate::PrimitiveKind::Box,
                size: [1.0, 1.0, 1.0],
                translation: [0.0, 0.0, 0.0],
                rotation: None,
                frame: None,
            },
            Op::Primitive {
                name: "b".into(),
                kind: crate::PrimitiveKind::Sphere,
                size: [0.5, 0.0, 0.0],
                translation: [0.0, 0.0, 0.0],
                rotation: None,
                frame: None,
            },
            Op::Primitive {
                name: "a".into(),
                kind: crate::PrimitiveKind::Cylinder,
                size: [0.1, 2.0, 0.0],
                translation: [0.0, 0.0, 0.0],
                rotation: None,
                frame: None,
            },
        ];
        let parts = evaluate(&ops).unwrap_or_else(|e| panic!("{e}"));
        let names: Vec<_> = parts.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["a", "b"]);
        assert!(parts[0].verts.len() > 4);
    }

    #[test]
    fn radial_array_circles_around_the_center() {
        let parts = evaluate(&[
            Op::Primitive {
                name: "petal".into(),
                kind: crate::PrimitiveKind::Sphere,
                size: [0.1, 0.0, 0.0],
                translation: [0.0, 0.0, 0.5],
                rotation: None,
                frame: None,
            },
            Op::RadialArray {
                name: "ring".into(),
                input: "petal".into(),
                count: 4,
                center: [0.0, 0.0, 0.0],
                degrees: 360.0,
                start_degrees: 0.0,
                axis: crate::Axis::Y,
                include_source: false,
            },
        ])
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(parts.len(), 5);
        // ring_0 is the unrotated copy; 90° steps walk +x, -z, -x.
        let xs: Vec<_> = parts[1..].iter().map(|p| p.translation[0]).collect();
        for (got, want) in xs.iter().zip([0.0, 0.5, 0.0, -0.5]) {
            assert!((got - want).abs() < 1e-5, "x {got} vs {want}");
        }
        let names: Vec<_> = parts.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["petal", "ring_0", "ring_1", "ring_2", "ring_3"]);
    }

    #[test]
    fn linear_array_steps_in_a_line() {
        let parts = evaluate(&[
            Op::Primitive {
                name: "post".into(),
                kind: crate::PrimitiveKind::Cylinder,
                size: [0.05, 1.0, 0.0],
                translation: [0.0, 0.5, 0.0],
                rotation: None,
                frame: None,
            },
            Op::LinearArray {
                name: "fence".into(),
                input: "post".into(),
                count: 4,
                offset: [1.0, 0.0, 0.0],
                local: false,
            },
        ])
        .unwrap_or_else(|e| panic!("{e}"));
        let xs: Vec<_> = parts.iter().map(|p| p.translation[0]).collect();
        // post (copy 0) + fence_1..fence_3
        assert_eq!(xs, [0.0, 1.0, 2.0, 3.0]);
        assert_eq!(parts[3].name, "fence_3");
    }

    #[test]
    fn mirror_reflects_and_flips_winding() {
        let parts = evaluate(&[
            Op::Primitive {
                name: "arm".into(),
                kind: crate::PrimitiveKind::Box,
                size: [0.2, 0.2, 0.6],
                translation: [0.5, 0.0, 0.0],
                rotation: None,
                frame: None,
            },
            Op::Mirror {
                name: "arm_m".into(),
                input: "arm".into(),
                axis: crate::Axis::X,
                offset: [0.0, 0.0, 0.0],
            },
        ])
        .unwrap_or_else(|e| panic!("{e}"));
        let mirrored = &parts[1];
        // The translated +x box lands on -x.
        let min_x = mirrored.verts.iter().map(|v| v[0]).fold(f32::MAX, f32::min);
        assert!((min_x - -0.6).abs() < 1e-5, "min x {min_x}");
        // Normals still point outward: every normal must agree with the
        // direction from the box center to its vertex.
        for (v, n) in mirrored.verts.iter().zip(&mirrored.normals) {
            // Outward = from the mirrored box's center (-0.5, 0, 0).
            let outward = (Vec3::from(*v) - Vec3::new(-0.5, 0.0, 0.0)).normalize_or_zero();
            let dot = outward.dot(Vec3::from(*n));
            assert!(dot > 0.0, "normal points inward at {v:?}");
        }
    }

    #[test]
    fn unknown_axis_is_rejected_at_deserialization() {
        let bad = r#"{"mirror":{"name":"m","input":"p","axis":"w"}}"#;
        let err = serde_json::from_str::<Op>(bad).err().map(|e| e.to_string());
        assert!(err.is_some_and(|e| e.contains("axis") || e.contains("variant")));
        let good = r#"{"mirror":{"name":"m","input":"p","axis":"x"}}"#;
        assert!(serde_json::from_str::<Op>(good).is_ok());
        // Old spellings of boolean ops still load.
        let old = r#"{"boolean":{"name":"c","op":"subtract","a":"a","b":"b"}}"#;
        assert!(serde_json::from_str::<Op>(old).is_ok());
    }

    #[test]
    fn missing_input_names_the_break() {
        let err = match evaluate(&[Op::Transform {
            name: "x".into(),
            input: "ghost".into(),
            translation: None,
            rotation: None,
            scale: None,
            relative: false,
            pivot: None,
        }]) {
            Err(e) => e,
            Ok(_) => panic!("missing input must fail"),
        };
        assert!(err.to_string().contains("ghost"), "{err}");
    }

    #[test]
    fn describe_lists_each_op() {
        let text = describe(&[Op::RadialArray {
            name: "ring".into(),
            input: "petal".into(),
            count: 8,
            center: [0.0; 3],
            degrees: 360.0,
            start_degrees: 45.0,
            axis: crate::Axis::Y,
            include_source: false,
        }]);
        assert!(text.contains("8 x `petal`"), "{text}");
    }

    fn prim(name: &str) -> Op {
        Op::Primitive {
            name: name.into(),
            kind: crate::PrimitiveKind::Box,
            size: [1.0; 3],
            translation: [0.0; 3],
            rotation: None,
            frame: None,
        }
    }

    #[test]
    fn outputs_expand_array_names() {
        let radial = Op::RadialArray {
            name: "r".into(),
            input: "p".into(),
            count: 3,
            center: [0.0; 3],
            degrees: 360.0,
            start_degrees: 0.0,
            axis: crate::Axis::Y,
            include_source: false,
        };
        assert_eq!(outputs(&radial), ["r_0", "r_1", "r_2"]);
        let linear = Op::LinearArray {
            name: "l".into(),
            input: "p".into(),
            count: 3,
            offset: [1.0, 0.0, 0.0],
            local: false,
        };
        assert_eq!(outputs(&linear), ["l_1", "l_2"]);
    }

    #[test]
    fn op_reading_array_output_is_ordered_after_the_array() {
        let parts = evaluate(&[
            Op::Transform {
                name: "moved".into(),
                input: "arr_1".into(),
                translation: Some([0.0, 5.0, 0.0]),
                rotation: None,
                scale: None,
                relative: false,
                pivot: None,
            },
            prim("p"),
            Op::LinearArray {
                name: "arr".into(),
                input: "p".into(),
                count: 3,
                offset: [2.0, 0.0, 0.0],
                local: false,
            },
        ])
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(parts.iter().any(|p| p.name == "moved"));
    }

    #[test]
    fn cache_reuses_unchanged_ops_and_invalidates_downstream() {
        let make = |size: f32| {
            vec![
                prim("cache_a"),
                Op::Transform {
                    name: "cache_b".into(),
                    input: "cache_a".into(),
                    translation: Some([0.0, size, 0.0]),
                    rotation: None,
                    scale: None,
                    relative: false,
                    pivot: None,
                },
                Op::Material {
                    name: "cache_c".into(),
                    input: "cache_b".into(),
                    base_color: Some([1.0, 0.0, 0.0, 1.0]),
                    roughness: None,
                    metalness: None,
                    pbr: None,
                    flat: false,
                },
            ]
        };
        let before = ops_run_on_thread();
        let first = evaluate(&make(7.25)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(ops_run_on_thread() - before, 3);
        // Identical graph: everything is served from the cache.
        let again = evaluate(&make(7.25)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(ops_run_on_thread() - before, 3, "second eval re-ran ops");
        assert_eq!(first.len(), again.len());
        assert_eq!(first[2].material.base_color, again[2].material.base_color);
        // Tweak the middle op: the primitive is reused, the other two re-run.
        let tweaked = evaluate(&make(8.5)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(ops_run_on_thread() - before, 5);
        assert_eq!(tweaked[1].translation, [0.0, 8.5, 0.0]);
        // The uncached path always re-derives and matches.
        let fresh = evaluate_uncached(&make(8.5)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(ops_run_on_thread() - before, 8, "uncached re-runs every op");
        assert_eq!(fresh[1].translation, tweaked[1].translation);
    }

    #[test]
    fn relative_transform_offsets_from_the_input() {
        let ops = vec![
            Op::Primitive {
                name: "a".into(),
                kind: crate::PrimitiveKind::Box,
                size: [1.0; 3],
                translation: [1.0, 2.0, 3.0],
                rotation: None,
                frame: None,
            },
            Op::Transform {
                name: "b".into(),
                input: "a".into(),
                translation: Some([0.5, 0.0, 0.0]),
                rotation: None,
                scale: Some([2.0, 2.0, 2.0]),
                relative: true,
                pivot: None,
            },
        ];
        let parts = evaluate_uncached(&ops).unwrap_or_else(|e| panic!("{e}"));
        let b = parts
            .iter()
            .find(|p| p.name == "b")
            .unwrap_or_else(|| panic!("b"));
        assert_eq!(b.translation, [1.5, 2.0, 3.0]);
        assert_eq!(b.scale, [2.0, 2.0, 2.0]);
    }
}
