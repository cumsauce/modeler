//! The procedural op graph: an ordered list of [`Op`] steps that derive the
//! model's parts from each other, evaluated top to bottom. An op may reference
//! earlier outputs by name; re-issuing an op with the same output name replaces
//! it (that is how parameters are "tweaked"). No game code.

use crate::{ModelerError, Op, Part, primitives, quat_of, uv};
use bevy::math::{Quat, Vec3};
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;

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
pub fn evaluate(ops: &[Op]) -> Result<Vec<Part>, ModelerError> {
    evaluate_with(ops, true)
}

/// Like [`evaluate`] but ignores and never fills the cache: always a fresh
/// derivation, for `check_rebuild` (a cached result would hide
/// non-determinism and stale-cache bugs).
pub fn evaluate_uncached(ops: &[Op]) -> Result<Vec<Part>, ModelerError> {
    evaluate_with(ops, false)
}

/// Cap on cached op results; the cache is dropped wholesale when exceeded
/// (cheap, and the working set re-fills as the graph is re-evaluated).
const CACHE_MAX_ENTRIES: usize = 2048;

type OpCache = HashMap<u64, Vec<(String, Part)>>;

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

fn evaluate_with(ops: &[Op], use_cache: bool) -> Result<Vec<Part>, ModelerError> {
    let order = eval_order(ops)?;
    let mut named: Vec<(String, Part)> = Vec::new();
    // Content hash of each named part: the key of the op run that wrote it,
    // combined with the output name.
    let mut hashes: HashMap<String, u64> = HashMap::new();
    for i in order {
        let op = &ops[i];
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
                    upsert(&mut named, &name, part);
                }
            }
            None => {
                RUNS.with(|r| r.set(r.get() + 1));
                run_op(op, &mut named)?;
                if use_cache && !matches!(op, Op::Rename { .. }) {
                    let produced: Vec<(String, Part)> = outs
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
    Ok(named.into_iter().map(|(_, part)| part).collect())
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
fn run_op(op: &Op, named: &mut Vec<(String, Part)>) -> Result<(), ModelerError> {
    match op {
        Op::Primitive {
            name,
            kind,
            size,
            translation,
            rotation,
        } => {
            let mut part =
                primitives::primitive(*kind, *size, name).map_err(|e| format!("{name}: {e}"))?;
            part.translation = *translation;
            if let Some(r) = rotation {
                part.rotation = *r;
            }
            upsert(named, name, part);
        }
        Op::Transform {
            name,
            input,
            translation,
            rotation,
            scale,
        } => {
            let mut part = lookup(named, input)?;
            part.name = name.clone();
            if name != input {
                part.visible = true;
            }
            if let Some(t) = translation {
                part.translation = *t;
            }
            if let Some(r) = rotation {
                part.rotation = *r;
            }
            if let Some(s) = scale {
                part.scale = *s;
            }
            upsert(named, name, part);
        }
        Op::Boolean { name, op, a, b } => {
            // `csg::boolean` looks parts up on a model and bakes their
            // transforms into the verts, so hand it a scratch pair.
            let mut scratch = crate::Model::named("scratch");
            scratch.parts = vec![
                std::sync::Arc::new(lookup(named, a)?),
                std::sync::Arc::new(lookup(named, b)?),
            ];
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
        } => {
            let mut part = lookup(named, input)?;
            part.name = name.clone();
            if name != input {
                part.visible = true;
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
            upsert(named, name, part);
        }
        Op::LinearArray {
            name,
            input,
            count,
            offset,
        } => {
            let source = lookup(named, input)?;
            if *count == 0 {
                return Err(format!("{name}: count must be at least 1").into());
            }
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
        Op::Rename { name, input } => {
            let mut part = lookup(named, input)?;
            part.name = name.clone();
            // Take the old name out (keeping its slot) before inserting.
            if let Some(slot) = named.iter().position(|(n, _)| n == input) {
                named.remove(slot);
                named.insert(slot.min(named.len()), (name.clone(), part));
            } else {
                upsert(named, name, part);
            }
        }
    }
    Ok(())
}

fn lookup(named: &[(String, Part)], name: &str) -> Result<Part, ModelerError> {
    named
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, p)| p.clone())
        .ok_or_else(|| ModelerError::MissingInput(name.to_string()))
}

/// Inserts `part` under `name`, replacing an earlier op's output of the same
/// name in place (so output order stays stable across re-issues).
fn upsert(named: &mut Vec<(String, Part)>, name: &str, part: Part) {
    match named.iter_mut().find(|(n, _)| n == name) {
        Some(slot) => slot.1 = part,
        None => named.push((name.to_string(), part)),
    }
}

/// The output part name an op writes, used for upsert matching.
pub fn output(op: &Op) -> &str {
    match op {
        Op::Primitive { name, .. }
        | Op::Transform { name, .. }
        | Op::Boolean { name, .. }
        | Op::RadialArray { name, .. }
        | Op::Material { name, .. }
        | Op::LinearArray { name, .. }
        | Op::Mirror { name, .. }
        | Op::Visibility { name, .. }
        | Op::Rename { name, .. } => name,
    }
}

/// Every part name an op writes: the array ops expand to `name_0..` (radial)
/// or `name_1..` (linear, the source being copy 0); everything else writes
/// just `name`. [`output`] stays the op's identity key for upsert matching.
pub fn outputs(op: &Op) -> Vec<String> {
    match op {
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
        other => vec![output(other).to_string()],
    }
}

/// What kind of op this is; upserts only replace ops of the same kind, so an
/// in-place op (output name == an input name) never eats its own source.
pub fn kind(op: &Op) -> &'static str {
    match op {
        Op::Primitive { .. } => "primitive",
        Op::Transform { .. } => "transform",
        Op::Boolean { .. } => "boolean",
        Op::RadialArray { .. } => "radial",
        Op::Material { .. } => "material",
        Op::LinearArray { .. } => "linear",
        Op::Mirror { .. } => "mirror",
        Op::Visibility { .. } => "visibility",
        Op::Rename { .. } => "rename",
    }
}

/// The part names an op reads.
pub fn inputs(op: &Op) -> Vec<&str> {
    match op {
        Op::Primitive { .. } => Vec::new(),
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
    }
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
            Op::Primitive {
                name,
                kind,
                size,
                translation,
                rotation,
            } => format!(
                "#{i} primitive `{name}` ({kind}) size={size:?} at={translation:?}{}",
                rotation.map_or_else(String::new, |r| format!(" rot={r:?}"))
            ),
            Op::Transform {
                name,
                input,
                translation,
                rotation,
                scale,
            } => {
                let mut s = format!("#{i} transform `{name}` <- `{input}`");
                if let Some(t) = translation {
                    s += &format!(" t={t:?}");
                }
                if let Some(r) = rotation {
                    s += &format!(" rot={r:?}");
                }
                if let Some(sc) = scale {
                    s += &format!(" scale={sc:?}");
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
            } => {
                let mut s = format!("#{i} material `{name}` <- `{input}`");
                if let Some(c) = base_color {
                    s += &format!(" color={c:?}");
                }
                if let Some(r) = roughness {
                    s += &format!(" roughness={r}");
                }
                if let Some(m) = metalness {
                    s += &format!(" metalness={m}");
                }
                s
            }
            Op::LinearArray {
                name,
                input,
                count,
                offset,
            } => format!("#{i} linear `{name}` = {count} x `{input}` stepping {offset:?}"),
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
        });
    }
    lines.join("\n")
}

/// The full describe line for a single op, looked up by output part name (the
/// newest op wins, matching upsert semantics). For the `op_get` tool.
pub fn describe_one(ops: &[Op], name: &str) -> Option<String> {
    ops.iter().rev().find(|op| output(op) == name).map(|op| {
        let d = describe(std::slice::from_ref(op));
        d.lines()
            .nth(1)
            .map(|l| l.replacen("#0 ", "", 1).to_string())
            .unwrap_or(d)
    })
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
            },
            Op::Transform {
                name: "lifted".into(),
                input: "box".into(),
                translation: Some([0.0, 2.0, 0.0]),
                rotation: None,
                scale: None,
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
            },
            Op::Primitive {
                name: "leg_template".into(),
                kind: crate::PrimitiveKind::Box,
                size: [0.08, 0.75, 0.08],
                translation: [0.0, 0.0, 0.0],
                rotation: None,
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
            },
            Op::Transform {
                name: "b".into(),
                input: "a".into(),
                translation: None,
                rotation: None,
                scale: None,
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
            },
            Op::Primitive {
                name: "b".into(),
                kind: crate::PrimitiveKind::Sphere,
                size: [0.5, 0.0, 0.0],
                translation: [0.0, 0.0, 0.0],
                rotation: None,
            },
            Op::Primitive {
                name: "a".into(),
                kind: crate::PrimitiveKind::Cylinder,
                size: [0.1, 2.0, 0.0],
                translation: [0.0, 0.0, 0.0],
                rotation: None,
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
            },
            Op::LinearArray {
                name: "fence".into(),
                input: "post".into(),
                count: 4,
                offset: [1.0, 0.0, 0.0],
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
            },
            prim("p"),
            Op::LinearArray {
                name: "arr".into(),
                input: "p".into(),
                count: 3,
                offset: [2.0, 0.0, 0.0],
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
                },
                Op::Material {
                    name: "cache_c".into(),
                    input: "cache_b".into(),
                    base_color: Some([1.0, 0.0, 0.0, 1.0]),
                    roughness: None,
                    metalness: None,
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
}
