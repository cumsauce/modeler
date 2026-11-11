//! The tool operations as plain functions on a [`Session`] (model + undo history). Pure,
//! unit-testable, wiring-free API used by both the MCP tools and tests.

use crate::{Annotation, Axis, BoolOp, Model, ModelerError, Op, Part, PrimitiveKind, graph};
use crate::{Quat, Vec3};

pub type CmdResult = Result<String, ModelerError>;

/// One item in an `apply_batch` call: either a graph op or a baked edit
/// (hide/show/rename) that runs in batch position.
#[derive(Debug)]
pub enum BatchItem {
    Op(Op),
    Hide {
        part: String,
    },
    Show {
        part: String,
    },
    Rename {
        part: String,
        to: String,
    },
    /// Remove the op producing `name` (or a baked part of that name).
    Delete {
        name: String,
    },
    /// A baked raw-triangle part.
    TriMesh {
        name: String,
        verts: Vec<[f32; 3]>,
        indices: Vec<u32>,
        normals: Option<Vec<[f32; 3]>>,
        uvs: Option<Vec<[f32; 2]>>,
    },
    /// Re-project a part's UVs (baked).
    Uvs {
        part: String,
        projection: String,
    },
    /// One rigid relative transform over many parts (names / `*` globs).
    Move {
        parts: Vec<String>,
        translation: Option<[f32; 3]>,
        rotation: Option<[f32; 4]>,
        pivot: Option<[f32; 3]>,
    },
}

/// Deterministic checks for `verify` and an `apply_ops` expect block. Each
/// field is optional; only the given checks run.
#[derive(Clone, Debug, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VerifySpec {
    /// Exact visible part names (order-insensitive); a trailing `*` matches
    /// a prefix (`"gz.*"`).
    #[serde(default)]
    pub parts: Option<Vec<String>>,
    /// Names that must be visible parts; extras are fine. Trailing `*` = prefix.
    #[serde(default)]
    pub includes: Option<Vec<String>>,
    /// Every visible part touches the ground (y = 0) or another part.
    #[serde(default)]
    pub grounded: Option<bool>,
    /// World bounds of visible parts; `null` entries skip an axis.
    #[serde(default)]
    pub bounds: Option<BoundsSpec>,
    /// Mesh checks: every visible part closed and outward-wound.
    #[serde(default)]
    pub validate: Option<bool>,
    /// Re-derive the graph uncached and diff against the cached parts.
    #[serde(default)]
    pub rebuild: Option<bool>,
    /// The centre of mass sits over the footprint (the model does not tip
    /// over), with uniform density.
    #[serde(default)]
    pub stable: Option<bool>,
}

#[derive(Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BoundsSpec {
    #[serde(default)]
    pub min: [Option<f32>; 3],
    #[serde(default)]
    pub max: [Option<f32>; 3],
    /// Allowed deviation per checked axis. Default 0.01.
    #[serde(default = "default_bounds_tol")]
    pub tol: f32,
}

fn default_bounds_tol() -> f32 {
    0.01
}

/// A spot in the live view a human clicked on, waiting for its comment text. Set by
/// the preview app; the next all-optional `add_annotation` consumes it.
#[derive(Clone, Debug)]
pub struct PendingPin {
    pub part: String,
    pub face: Option<u32>,
    pub vertex: Option<u32>,
    pub position: [f32; 3],
}

/// The modeling session the MCP tools drive.
pub struct Session {
    pub model: Model,
    /// The human's last click on the model, if it hasn't been commented yet.
    pub pending_pin: Option<PendingPin>,
    /// Nesting depth of [`Session::batch`]; while > 0, commits are collected
    /// into one history step recorded when the outermost batch ends.
    batch_depth: usize,
    batch_labels: Vec<String>,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            model: Model::named("model"),
            pending_pin: None,
            batch_depth: 0,
            batch_labels: Vec::new(),
        }
    }
}

impl Session {
    /// Cheap summary of everything the autosave persists; if it is unchanged
    /// across a call, nothing needs writing.
    pub fn persist_fingerprint(&self) -> (u64, usize, Option<usize>, usize, u32, String) {
        (
            self.model.version,
            self.model.history.nodes.len(),
            self.model.history.current,
            self.model.annotations.len(),
            self.model.next_annotation_id,
            self.model.name.clone(),
        )
    }

    /// Records the just-mutated model in the history tree under `label`. The
    /// tree lives on the model, so it travels with saves and prefab loads.
    fn commit(&mut self, label: &str) {
        if self.batch_depth > 0 {
            self.model.version += 1;
            self.batch_labels.push(label.to_string());
            return;
        }
        self.record(label);
    }

    fn record(&mut self, label: &str) {
        self.model.version += 1;
        // The snapshot carries no tree of its own: the live history on the
        // session model is the single source of truth for the tree shape.
        let mut snapshot = self.model.clone();
        snapshot.history = crate::History::default();
        self.model.history.record(snapshot, label);
    }

    /// Runs `f` as one history step: every commit inside it (including the
    /// earlier items when a later one fails) lands as a single undoable node.
    pub fn batch<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, ModelerError>,
    ) -> Result<T, ModelerError> {
        self.batch_depth += 1;
        let r = f(self);
        self.batch_depth -= 1;
        if self.batch_depth == 0 && !self.batch_labels.is_empty() {
            let labels = std::mem::take(&mut self.batch_labels);
            let label = match labels.as_slice() {
                [one] => one.clone(),
                [first, .., last] => format!("{} edits: {first} … {last}", labels.len()),
                [] => String::new(),
            };
            // The counter already advanced per edit; record() bumps it once more.
            self.model.version -= 1;
            self.record(&label);
        }
        r
    }

    /// Upserts one op into the graph, re-derives the parts and records history.
    /// Re-issuing an op replaces the newest op of the same kind and output name
    /// — never one it reads from, so in-place tweaks (`petal` <- `petal`) are
    /// safe and append instead.
    fn apply_op(&mut self, op: Op, label: &str) -> CmdResult {
        let out = graph::output(&op).to_string();
        let kind = graph::kind(&op);
        let reads = graph::inputs(&op);
        // A visibility edit is in place by nature (reads and writes the same
        // name), yet re-issuing it must replace the previous one, not stack.
        let replace_in_place = kind == "visibility";
        // "Never replace an op this one reads from" compares the old op's
        // actual outputs: an array named after its source (`tile` -> tile_1..)
        // writes tile_1.., not `tile`, so re-issuing it replaces it.
        let prev = self.model.ops.iter().rposition(|o| {
            graph::output(o) == out
                && graph::kind(o) == kind
                && (replace_in_place
                    || !graph::outputs(o)
                        .iter()
                        .any(|x| reads.contains(&x.as_str())))
        });
        // Reject outputs that collide with another op's outputs (e.g. a
        // primitive `ring_0` vs a radial array `ring`); in-place chains (an op
        // that reads the name it writes) are legitimate and exempt.
        let mine = graph::outputs(&op);
        for (i, other) in self.model.ops.iter().enumerate() {
            if Some(i) == prev {
                continue;
            }
            let other_reads = graph::inputs(other);
            for n in graph::outputs(other) {
                if mine.contains(&n)
                    && !reads.contains(&n.as_str())
                    && !other_reads.contains(&n.as_str())
                {
                    return Err(format!(
                        "`{n}` is already produced by another op ({}); choose a different name",
                        graph::output(other)
                    )
                    .into());
                }
            }
        }
        let old = prev.map(|i| self.model.ops[i].clone());
        match prev {
            Some(i) => self.model.ops[i] = op,
            None => self.model.ops.push(op),
        }
        if let Err(e) = self.model.refresh() {
            // Never leave the graph in a state that does not evaluate.
            match (prev, old) {
                (Some(i), Some(old_op)) => self.model.ops[i] = old_op,
                (None, _) => {
                    self.model.ops.pop();
                }
                (Some(_), None) => {}
            }
            return Err(e);
        }
        self.commit(label);
        Ok(format!("{label} ok"))
    }

    /// A primitive as a graph step (upsert by output name).
    pub fn op_primitive(
        &mut self,
        name: &str,
        kind: PrimitiveKind,
        size: [f32; 3],
        translation: [f32; 3],
        rotation: Option<[f32; 4]>,
    ) -> CmdResult {
        self.apply_op(
            Op::Primitive {
                name: name.into(),
                kind,
                size,
                translation,
                rotation,
            },
            &format!("op primitive `{name}` ({kind})"),
        )
    }

    /// Transform an earlier part into a new output.
    pub fn op_transform(
        &mut self,
        name: &str,
        input: &str,
        translation: Option<[f32; 3]>,
        rotation: Option<[f32; 4]>,
        scale: Option<[f32; 3]>,
    ) -> CmdResult {
        self.apply_op(
            Op::Transform {
                name: name.into(),
                input: input.into(),
                translation,
                rotation,
                scale,
                relative: false,
                pivot: None,
            },
            &format!("op transform `{name}` <- `{input}`"),
        )
    }

    pub fn op_boolean(&mut self, name: &str, op: BoolOp, a: &str, b: &str) -> CmdResult {
        self.apply_op(
            Op::Boolean {
                name: name.into(),
                op,
                a: a.into(),
                b: b.into(),
            },
            &format!("op boolean `{name}` = {op}({a}, {b})"),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn op_radial_array(
        &mut self,
        name: &str,
        input: &str,
        count: u32,
        center: [f32; 3],
        degrees: f32,
        start_degrees: f32,
        axis: Axis,
        include_source: bool,
    ) -> CmdResult {
        self.apply_op(
            Op::RadialArray {
                name: name.into(),
                input: input.into(),
                count,
                center,
                degrees,
                start_degrees,
                axis,
                include_source,
            },
            &format!("op radial `{name}` = {count} x `{input}` over {degrees}° about {axis}"),
        )
    }

    pub fn op_material(
        &mut self,
        name: &str,
        input: &str,
        base_color: Option<[f32; 4]>,
        roughness: Option<f32>,
        metalness: Option<f32>,
    ) -> CmdResult {
        self.apply_op(
            Op::Material {
                name: name.into(),
                input: input.into(),
                base_color,
                roughness,
                metalness,
            },
            &format!("op material `{name}` <- `{input}`"),
        )
    }

    /// Copies an earlier part in a straight line.
    pub fn op_linear_array(
        &mut self,
        name: &str,
        input: &str,
        count: u32,
        offset: [f32; 3],
    ) -> CmdResult {
        self.apply_op(
            Op::LinearArray {
                name: name.into(),
                input: input.into(),
                count,
                offset,
            },
            &format!("op linear `{name}` = {count} x `{input}`"),
        )
    }

    /// Mirrors an earlier part across a plane (axis x/y/z at offset).
    pub fn op_mirror(
        &mut self,
        name: &str,
        input: &str,
        axis: Axis,
        offset: [f32; 3],
    ) -> CmdResult {
        self.apply_op(
            Op::Mirror {
                name: name.into(),
                input: input.into(),
                axis,
                offset,
            },
            &format!("op mirror `{name}` = `{input}` across {axis}"),
        )
    }

    /// Removes the op whose output is `name` and re-derives.
    pub fn op_delete(&mut self, name: &str) -> CmdResult {
        let Some(idx) = self.model.ops.iter().position(|o| graph::output(o) == name) else {
            return Err(format!("no op with output `{name}`").into());
        };
        let produced = graph::outputs(&self.model.ops[idx]);
        let dependents: Vec<String> = self
            .model
            .ops
            .iter()
            .enumerate()
            .filter(|&(i, o)| {
                // In-place edits of the deleted part (hide, material,
                // transform on itself) go with it.
                let own_edit = graph::inputs(o) == [graph::output(o).to_string()]
                    && produced.iter().any(|p| p == graph::output(o));
                i != idx
                    && !own_edit
                    && graph::inputs(o)
                        .iter()
                        .any(|inp| produced.iter().any(|p| p == inp))
            })
            .map(|(_, o)| format!("`{}`", graph::output(o)))
            .collect();
        if !dependents.is_empty() {
            return Err(format!(
                "can't delete `{name}`: still read by {}; delete or repoint those first",
                dependents.join(", ")
            )
            .into());
        }
        self.model.ops.retain(|o| {
            let own_edit = graph::inputs(o) == [graph::output(o).to_string()]
                && produced.iter().any(|p| p == graph::output(o));
            graph::output(o) != name && !own_edit
        });
        let live: Vec<String> = self.model.ops.iter().map(Self::template_key).collect();
        self.model.templates.retain(|k, _| live.contains(k));
        self.model.refresh()?;
        self.commit(&format!("op delete `{name}`"));
        Ok(format!("deleted op `{name}`"))
    }

    /// The op graph, one line per step.
    pub fn describe_ops(&self) -> String {
        graph::describe(&self.model.ops)
    }

    /// The describe line for a single op, used as its history label.
    fn op_label(op: &Op) -> String {
        graph::describe(std::slice::from_ref(op))
            .lines()
            .nth(1)
            .map(|l| l.replacen("#0 ", "", 1).to_string())
            .unwrap_or_else(|| "op".into())
    }

    /// Replaces the session with a fresh empty model. The old model is recorded
    /// in the (carried-over) history tree first, so it is one `undo` away —
    /// reset without losing work.
    pub fn new_model(&mut self, name: &str) -> CmdResult {
        let snapshot = self.model.clone();
        let mut model = Model::named(name);
        model.history = std::mem::take(&mut self.model.history);
        model.version = self.model.version + 1;
        model
            .history
            .record(snapshot, format!("new model `{name}`"));
        self.pending_pin = None;
        self.model = model;
        Ok(format!(
            "started new model `{name}` (the old model is one undo away)"
        ))
    }

    /// Applies a batch of ops in order, one graph application each (same
    /// upsert/rollback semantics as the individual op tools). Stops at the
    /// first error with its index; the ops before it stay applied.
    pub fn apply_ops(&mut self, ops: Vec<Op>) -> CmdResult {
        self.apply_batch(ops.into_iter().map(BatchItem::Op).collect())
    }

    /// The `apply_ops` entry point: raw JSON items plus optional parameter
    /// updates. Parameters are merged first; every item is resolved against
    /// them and parsed before anything is applied (a malformed batch changes
    /// nothing). Ops whose items reference parameters keep their raw item as a
    /// template, so a later parameter change re-derives them.
    pub fn apply_values(
        &mut self,
        params: Option<crate::params::Params>,
        items: Vec<serde_json::Value>,
    ) -> CmdResult {
        self.apply_values_as(params, items, true)
    }

    /// [`Self::apply_values`] ending in the full scene summary (`full`) or a
    /// brief one: counts, bounds, which parts were added / changed / removed,
    /// and the warnings. The brief form keeps a long build's context small.
    pub fn apply_values_as(
        &mut self,
        params: Option<crate::params::Params>,
        items: Vec<serde_json::Value>,
        full: bool,
    ) -> CmdResult {
        let before = self.part_fingerprints();
        let text = self.batch(|s| s.apply_values_inner(params, items))?;
        if full {
            return Ok(format!("{text}\n{}", self.summary()));
        }
        // Brief: routine per-item echoes collapse to a count; anything else
        // (param lines, notes) stays.
        let routine = |l: &str| {
            l.starts_with('#')
                && (l.ends_with(" ok") || l.ends_with(" hidden") || l.ends_with(" shown"))
        };
        let n = text.lines().filter(|l| routine(l)).count();
        let mut kept: Vec<&str> = text.lines().filter(|l| !routine(l)).collect();
        let applied = format!("applied {n} item(s)");
        kept.insert(0, &applied);
        Ok(format!(
            "{}\n{}",
            kept.join("\n"),
            self.brief_summary(&before)
        ))
    }

    /// A content hash per part (geometry, transform, material, visibility),
    /// to tell which parts a call touched.
    pub fn part_fingerprints(&self) -> std::collections::BTreeMap<String, u64> {
        use std::hash::{Hash, Hasher};
        self.model
            .parts
            .iter()
            .map(|p| {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                for v in &p.verts {
                    v.iter().for_each(|f| f.to_bits().hash(&mut h));
                }
                p.indices.hash(&mut h);
                for f in p
                    .translation
                    .iter()
                    .chain(&p.rotation)
                    .chain(&p.scale)
                    .chain(&p.material.base_color)
                {
                    f.to_bits().hash(&mut h);
                }
                p.material.roughness.to_bits().hash(&mut h);
                p.material.metalness.to_bits().hash(&mut h);
                p.visible.hash(&mut h);
                (p.name.clone(), h.finish())
            })
            .collect()
    }

    /// The short result tail for `apply_ops`: model line, the parts this call
    /// added (+), changed (~) and removed (-), and every warning.
    pub fn brief_summary(&self, before: &std::collections::BTreeMap<String, u64>) -> String {
        let after = self.part_fingerprints();
        let bounds = self
            .model
            .bounds()
            .map(|(min, max)| format!("bounds {min:?}..{max:?}"))
            .unwrap_or_else(|| "empty".into());
        let mut s = format!(
            "model `{}` v{}: {} parts, {} ops, {bounds}\n",
            self.model.name,
            self.model.version,
            self.model.parts.len(),
            self.model.ops.len()
        );
        let mut changes = Vec::new();
        for (n, h) in &after {
            match before.get(n) {
                None => changes.push(format!("+{n}")),
                Some(b) if b != h => changes.push(format!("~{n}")),
                _ => {}
            }
        }
        for n in before.keys() {
            if !after.contains_key(n) {
                changes.push(format!("-{n}"));
            }
        }
        if changes.is_empty() {
            s += "parts: no change\n";
        } else {
            let shown = changes.len().min(40);
            s += &format!(
                "parts ({} changed): {}{}\n",
                changes.len(),
                changes[..shown].join(", "),
                if changes.len() > shown { ", ..." } else { "" }
            );
        }
        if self.pending_pin.is_some() {
            s += "a pin is waiting in the live view: use add_annotation without face/vertex/position to comment on it\n";
        }
        let lints = self.lints();
        if lints.is_empty() {
            s += "warnings: none\n";
        } else {
            s += &format!("warnings ({}):\n", lints.len());
            for l in &lints {
                s += &format!("- {l}\n");
            }
        }
        s += "(brief result: scene_summary lists every part; pass summary: \"full\" to get it here)\n";
        s
    }

    fn apply_values_inner(
        &mut self,
        params: Option<crate::params::Params>,
        items: Vec<serde_json::Value>,
    ) -> CmdResult {
        // Drop templates whose op is gone (deleted, or failed to apply).
        let live: Vec<String> = self.model.ops.iter().map(Self::template_key).collect();
        self.model.templates.retain(|k, _| live.contains(k));
        let mut merged = self.model.params.clone();
        let mut changed: Vec<String> = Vec::new();
        for (k, v) in params.unwrap_or_default() {
            if merged.get(&k) != Some(&v) {
                changed.push(k.clone());
            }
            merged.insert(k, v);
        }
        // Parse everything first, against the merged parameters.
        let mut parsed = Vec::with_capacity(items.len());
        for (i, raw) in items.iter().enumerate() {
            let resolved = crate::params::resolve(raw, &merged)
                .map_err(|e| format!("op #{i}: {e}\n(nothing was applied)"))?;
            let item = crate::spec::parse_item(i, &resolved)
                .map_err(|e| format!("{e}\n(nothing was applied)"))?;
            let template = (!crate::params::refs(raw).is_empty()).then(|| raw.clone());
            parsed.push((item, template));
        }
        // Templated ops already in the graph that read a changed parameter
        // (and are not re-issued by this batch) are re-derived first.
        let reissued: Vec<String> = parsed
            .iter()
            .filter_map(|(it, _)| match it {
                BatchItem::Op(op) => Some(Self::template_key(op)),
                _ => None,
            })
            .collect();
        let mut stale = Vec::new();
        for (name, raw) in &self.model.templates {
            if reissued.contains(name) {
                continue;
            }
            if crate::params::refs(raw).iter().any(|r| changed.contains(r)) {
                let resolved = crate::params::resolve(raw, &merged)
                    .map_err(|e| format!("re-deriving `{name}`: {e}"))?;
                let item = crate::spec::parse_item(0, &resolved)
                    .map_err(|e| format!("re-deriving `{name}`: {e}"))?;
                stale.push((item, Some(raw.clone())));
            }
        }
        let mut lines = Vec::new();
        if !changed.is_empty() {
            // One history step for the whole change: swap the dependent ops in
            // place, re-derive once, commit once (undo restores both).
            let before = self.model.clone();
            self.model.params = merged;
            let n = stale.len();
            for (item, _) in stale {
                if let BatchItem::Op(op) = item
                    && let Some(slot) = self.model.ops.iter_mut().find(|o| {
                        graph::output(o) == graph::output(&op) && graph::kind(o) == graph::kind(&op)
                    })
                {
                    *slot = op;
                }
            }
            if let Err(e) = self.model.refresh() {
                self.model = before;
                return Err(format!("re-deriving ops for changed params failed: {e}").into());
            }
            self.commit(&format!("params {}", changed.join(", ")));
            lines.push(format!(
                "params: {}",
                self.model
                    .params
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            if n > 0 {
                lines.push(format!("re-derived {n} op(s) that read changed params"));
            }
        }
        lines.push(self.apply_items(parsed, false)?);
        Ok(lines.join("\n"))
    }

    /// Applies a mixed batch: graph ops and baked edits (hide/show/rename),
    /// each running in its listed position with per-item error reporting.
    /// Graph ops keep the upsert/rollback semantics of `apply_op`.
    pub fn apply_batch(&mut self, items: Vec<BatchItem>) -> CmdResult {
        self.apply_items(items.into_iter().map(|i| (i, None)).collect(), true)
    }

    /// `apply_batch` with an optional parameter template per item: an op item
    /// stores its template (or drops a stale one) so parameter changes can
    /// re-derive it.
    fn apply_items(
        &mut self,
        items: Vec<(BatchItem, Option<serde_json::Value>)>,
        with_summary: bool,
    ) -> CmdResult {
        let mut lines = Vec::with_capacity(items.len());
        for (i, (item, template)) in items.into_iter().enumerate() {
            if let BatchItem::Op(op) = &item {
                let key = Self::template_key(op);
                match template {
                    Some(t) => {
                        self.model.templates.insert(key, t);
                    }
                    None => {
                        self.model.templates.remove(&key);
                    }
                }
            }
            let fail = |e: ModelerError| format!("batch item #{i} failed: {e}");
            let msg = match item {
                // In-place material/transform on a baked part: a baked edit.
                BatchItem::Op(Op::Material {
                    name,
                    input,
                    base_color,
                    roughness,
                    metalness,
                }) if name == input && !self.derived_by_ops(&name) => self
                    .set_material(&name, base_color, roughness, metalness)
                    .map_err(fail)?,
                BatchItem::Op(Op::Transform {
                    name,
                    input,
                    translation,
                    rotation,
                    scale,
                    relative: false,
                    ..
                }) if name == input && !self.derived_by_ops(&name) => self
                    .transform_part(&name, translation, rotation, scale)
                    .map_err(fail)?,
                BatchItem::Op(op) => {
                    let label = Self::op_label(&op);
                    self.apply_op(op, &label)
                        .map_err(|e| format!("batch item #{i} (`{label}`) failed: {e}"))?;
                    format!("{label} ok")
                }
                BatchItem::Delete { name } => {
                    if self.model.ops.iter().any(|o| graph::output(o) == name) {
                        self.op_delete(&name).map_err(fail)?
                    } else {
                        self.delete_part(&name).map_err(fail)?
                    }
                }
                BatchItem::TriMesh {
                    name,
                    verts,
                    indices,
                    normals,
                    uvs,
                } => self
                    .add_tri_mesh(&name, verts, indices, normals, uvs)
                    .map_err(fail)?,
                BatchItem::Move {
                    parts,
                    translation,
                    rotation,
                    pivot,
                } => self
                    .move_parts(&parts, translation, rotation, pivot)
                    .map_err(fail)?,
                BatchItem::Uvs { part, projection } => {
                    self.set_uvs(&part, &projection).map_err(fail)?
                }
                BatchItem::Hide { part } => self
                    .set_visible_glob(&part, false)
                    .map_err(|e| format!("batch item #{i} failed: {e}"))?,
                BatchItem::Show { part } => self
                    .set_visible_glob(&part, true)
                    .map_err(|e| format!("batch item #{i} failed: {e}"))?,
                BatchItem::Rename { part, to } => self
                    .rename_part(&part, &to)
                    .map_err(|e| format!("batch item #{i} failed: {e}"))?,
            };
            lines.push(format!("#{i} {msg}"));
        }
        if with_summary {
            lines.push(self.summary());
        }
        Ok(lines.join("\n"))
    }

    /// True when some op in the graph writes a part called `name`. Edits to
    /// such parts are recorded as graph ops (so they survive re-derivation);
    /// anything else is a baked part and is edited directly.
    fn derived_by_ops(&self, name: &str) -> bool {
        self.model.derived_from_ops
            && self
                .model
                .ops
                .iter()
                .any(|o| graph::outputs(o).iter().any(|n| n == name))
    }

    /// Moves every visible part matching `patterns` (exact or trailing-`*`)
    /// as one rigid body: rotation about `pivot` (default: centre of their
    /// combined bottom face), then translation. Op-derived parts get a
    /// relative in-place transform op each (so it survives re-derivation);
    /// baked parts are edited directly.
    pub fn move_parts(
        &mut self,
        patterns: &[String],
        translation: Option<[f32; 3]>,
        rotation: Option<[f32; 4]>,
        pivot: Option<[f32; 3]>,
    ) -> CmdResult {
        let matches = |n: &str| {
            patterns.iter().any(|p| match p.strip_suffix('*') {
                Some(pre) => n.starts_with(pre),
                None => n == p,
            })
        };
        let names: Vec<String> = self
            .model
            .parts
            .iter()
            .filter(|p| p.visible && matches(&p.name))
            .map(|p| p.name.clone())
            .collect();
        if names.is_empty() {
            return Err(format!("move: no visible part matches {patterns:?}").into());
        }
        let pivot = pivot.unwrap_or_else(|| {
            let (lo, hi) = self.model.bounds_of(&names).unwrap_or_default();
            [(lo.x + hi.x) * 0.5, lo.y, (lo.z + hi.z) * 0.5]
        });
        let r = rotation.map(crate::quat_of).unwrap_or(Quat::IDENTITY);
        let t = Vec3::from(translation.unwrap_or([0.0; 3]));
        let p = Vec3::from(pivot);
        for n in &names {
            if self.derived_by_ops(n) {
                self.model.ops.push(Op::Transform {
                    name: n.clone(),
                    input: n.clone(),
                    translation,
                    rotation,
                    scale: None,
                    relative: true,
                    pivot: rotation.map(|_| pivot),
                });
            } else if let Some(part) = self.model.parts.iter_mut().find(|q| &q.name == n) {
                let part = std::sync::Arc::make_mut(part);
                let tr = p + r * (Vec3::from(part.translation) - p) + t;
                part.translation = tr.to_array();
                part.rotation = (r * crate::quat_of(part.rotation)).to_array();
            }
        }
        self.model.refresh()?;
        self.commit(&format!("move {} part(s)", names.len()));
        Ok(format!(
            "moved {} part(s) as one body (pivot [{:.4}, {:.4}, {:.4}])",
            names.len(),
            pivot[0],
            pivot[1],
            pivot[2]
        ))
    }

    /// [`Self::set_visible`] on one part, or on every part matching a
    /// trailing-`*` prefix (`"book*"`, `"cut_*"`).
    pub fn set_visible_glob(&mut self, pattern: &str, visible: bool) -> CmdResult {
        let Some(prefix) = pattern.strip_suffix('*') else {
            return self.set_visible(pattern, visible);
        };
        let names: Vec<String> = self
            .model
            .parts
            .iter()
            .filter(|p| p.name.starts_with(prefix) && p.visible != visible)
            .map(|p| p.name.clone())
            .collect();
        if names.is_empty() && !self.model.parts.iter().any(|p| p.name.starts_with(prefix)) {
            return Err(format!("no part matches `{pattern}`").into());
        }
        for n in &names {
            self.set_visible(n, visible)?;
        }
        Ok(format!(
            "{} part(s) {} by `{pattern}`",
            names.len(),
            if visible { "shown" } else { "hidden" }
        ))
    }

    /// Shows or hides a part. Hidden parts stay in the model and keep
    /// deriving from ops — they are just skipped by the render and the glTF
    /// export. For op-derived parts this records a `Visibility` op, so the hide
    /// survives re-derivation; baked parts are edited directly.
    pub fn set_visible(&mut self, name: &str, visible: bool) -> CmdResult {
        if self.derived_by_ops(name) {
            let verb = if visible { "shown" } else { "hidden" };
            self.apply_op(
                Op::Visibility {
                    name: name.into(),
                    input: name.into(),
                    visible,
                },
                &format!("{verb} `{name}`"),
            )?;
            return Ok(format!("part `{name}` {verb}"));
        }
        let Some(p) = self.model.part_mut(name) else {
            return Err(ModelerError::NoPart(name.to_string()));
        };
        p.visible = visible;
        let verb = if visible { "shown" } else { "hidden" };
        self.commit(&format!("{verb} `{name}`"));
        Ok(format!("part `{name}` {verb}"))
    }

    /// The full parameter dump of one op, by output part name.
    pub fn describe_op(&self, name: &str) -> CmdResult {
        graph::describe_one(&self.model.ops, name)
            .ok_or_else(|| ModelerError::Invalid(format!("no op with output `{name}`")))
    }

    /// Re-derives the graph and diffs it against the cached parts (see
    /// []).
    pub fn check_rebuild(&self) -> CmdResult {
        crate::verify::check_rebuild(&self.model)
    }

    /// Exact visible-part-list assertion (see []).
    pub fn expect_parts(&self, expected: &[String]) -> CmdResult {
        crate::verify::expect_parts(&self.model, expected)
    }

    /// World-bounds assertion (see []).
    pub fn expect_bounds(
        &self,
        min: [Option<f32>; 3],
        max: [Option<f32>; 3],
        tol: f32,
    ) -> CmdResult {
        crate::verify::expect_bounds(&self.model, min, max, tol)
    }

    /// Per-part mesh validation (see []).
    /// Runs the checks in `spec` (all of validate + rebuild when `spec` is
    /// empty). Returns (all passed, one line per check).
    pub fn verify(&self, spec: &VerifySpec) -> (bool, String) {
        let checks = self.verify_checks(spec);
        let ok = checks.iter().all(|c| c.1);
        let lines: Vec<String> = checks
            .iter()
            .map(|(name, pass, detail)| {
                format!("{} {name}: {detail}", if *pass { "PASS" } else { "FAIL" })
            })
            .collect();
        (ok, lines.join("\n"))
    }

    /// The checks behind [`Self::verify`] as (name, passed, detail).
    pub fn verify_checks(&self, spec: &VerifySpec) -> Vec<(String, bool, String)> {
        let empty = spec.parts.is_none()
            && spec.includes.is_none()
            && spec.grounded.is_none()
            && spec.bounds.is_none()
            && spec.validate.is_none()
            && spec.rebuild.is_none()
            && spec.stable.is_none();
        let mut out = Vec::new();
        let mut record = |name: &str, r: Result<String, String>| match r {
            Ok(t) => out.push((name.to_string(), true, t)),
            Err(e) => out.push((name.to_string(), false, e)),
        };
        if let Some(parts) = &spec.parts {
            record("parts", self.expect_parts(parts).map_err(|e| e.to_string()));
        }
        if let Some(w) = &spec.includes {
            record(
                "includes",
                crate::verify::expect_includes(&self.model, w).map_err(|e| e.to_string()),
            );
        }
        if spec.grounded == Some(true) {
            record(
                "grounded",
                crate::verify::expect_grounded(&self.model).map_err(|e| e.to_string()),
            );
        }
        if let Some(b) = &spec.bounds {
            record(
                "bounds",
                self.expect_bounds(b.min, b.max, b.tol)
                    .map_err(|e| e.to_string()),
            );
        }
        if spec.validate.unwrap_or(empty) {
            let v = self.validate();
            if v.starts_with("all parts") || v.starts_with("no parts") {
                record("validate", Ok(v));
            } else {
                record("validate", Err(v.replace('\n', "; ")));
            }
        }
        if spec.rebuild.unwrap_or(empty) {
            record("rebuild", self.check_rebuild().map_err(|e| e.to_string()));
        }
        if spec.stable == Some(true) {
            let (text, json) = crate::inspect::mass(&self.model, &Default::default(), 1000.0);
            let first = text.lines().nth(1).unwrap_or("").to_string();
            if json["stability"]["stable"].as_bool() == Some(true) {
                record("stable", Ok(first));
            } else {
                record("stable", Err(first));
            }
        }
        out
    }

    /// Checks as JSON: [{"check", "pass", "detail"}].
    pub fn checks_json(checks: &[(String, bool, String)]) -> serde_json::Value {
        serde_json::Value::Array(
            checks
                .iter()
                .map(|(c, p, d)| serde_json::json!({"check": c, "pass": p, "detail": d}))
                .collect(),
        )
    }

    /// The scene summary as structured data: model, bounds, params, parts
    /// (visibility, triangle count, world bounds, material), ops (one line
    /// each), warnings and annotations.
    pub fn summary_json(&self) -> serde_json::Value {
        use serde_json::json;
        let b = |v: bevy::math::Vec3| [v.x, v.y, v.z];
        json!({
            "model": self.model.name,
            "version": self.model.version,
            "bounds": self.model.bounds().map(|(lo, hi)| json!({"min": b(lo), "max": b(hi)})),
            "params": self.model.params,
            "parts": self.model.parts.iter().map(|p| {
                let (lo, hi) = p.bounds();
                json!({
                    "name": p.name,
                    "visible": p.visible,
                    "tris": p.tri_count(),
                    "bounds": {"min": b(lo), "max": b(hi)},
                    "color": p.material.base_color,
                    "roughness": p.material.roughness,
                    "metalness": p.material.metalness,
                })
            }).collect::<Vec<_>>(),
            "ops": self.model.ops.iter().map(Self::op_label).collect::<Vec<_>>(),
            "warnings": self.lints(),
            "annotations": self.model.annotations.iter().map(|a| json!({
                "id": a.id, "part": a.part, "text": a.text, "face": a.face, "vertex": a.vertex,
            })).collect::<Vec<_>>(),
        })
    }

    pub fn validate(&self) -> String {
        crate::verify::validate(&self.model)
    }

    /// Deterministic scene warnings (see []).
    pub fn lints(&self) -> Vec<String> {
        crate::verify::lints(&self.model)
    }

    pub fn add_primitive(
        &mut self,
        kind: PrimitiveKind,
        size: [f32; 3],
        translation: [f32; 3],
        name: &str,
    ) -> CmdResult {
        let mut part = crate::primitives::primitive(kind, size, name).map_err(|e| e.to_string())?;
        part.translation = translation;
        self.model.parts.push(std::sync::Arc::new(part));
        self.model.mark_baked();
        self.commit(&format!("add {kind} `{name}`"));
        Ok(format!("added {kind} `{name}` at {translation:?}"))
    }

    /// Raw triangle mesh. Auto normals + box UV projection when omitted.
    pub fn add_tri_mesh(
        &mut self,
        name: &str,
        verts: Vec<[f32; 3]>,
        indices: Vec<u32>,
        normals: Option<Vec<[f32; 3]>>,
        uvs: Option<Vec<[f32; 2]>>,
    ) -> CmdResult {
        if verts.len() > u32::MAX as usize || !indices.len().is_multiple_of(3) {
            return Err("indices must be a multiple of 3".into());
        }
        if indices.iter().any(|i| *i as usize >= verts.len()) {
            return Err("index out of range".into());
        }
        let mut part = Part::new(name);
        part.verts = verts;
        part.indices = indices;
        crate::uv::ensure_normals(&mut part);
        crate::uv::ensure_uvs(&mut part, Some("box")).map_err(|e| e.to_string())?;
        if let Some(n) = normals {
            part.normals = n;
        }
        if let Some(u) = uvs {
            part.uvs = u;
        }
        let tris = part.tri_count();
        self.model.parts.push(std::sync::Arc::new(part));
        self.commit(&format!("add tri mesh `{name}`"));
        Ok(format!("added tri mesh `{name}` ({tris} tris)"))
    }

    pub fn boolean(&mut self, op: BoolOp, a: &str, b: &str, name: Option<&str>) -> CmdResult {
        let out = crate::csg::boolean(&self.model, op, a, b, name)?;
        self.model.parts.retain(|p| p.name != a && p.name != b);
        self.model.parts.push(std::sync::Arc::new(out));
        self.commit(&format!("{op}({a}, {b})"));
        Ok(format!("{op}({a}, {b}) ok"))
    }

    pub fn transform_part(
        &mut self,
        part: &str,
        translation: Option<[f32; 3]>,
        rotation: Option<[f32; 4]>,
        scale: Option<[f32; 3]>,
    ) -> CmdResult {
        if self.derived_by_ops(part) {
            self.op_transform(part, part, translation, rotation, scale)?;
            return Ok(format!("transformed `{part}`"));
        }
        let p = self
            .model
            .part_mut(part)
            .ok_or_else(|| ModelerError::NoPart(part.to_string()))?;
        if let Some(t) = translation {
            p.translation = t;
        }
        if let Some(r) = rotation {
            p.rotation = r;
        }
        if let Some(s) = scale {
            p.scale = s;
        }
        self.model.mark_baked();
        self.commit(&format!("transform `{part}`"));
        Ok(format!("transformed `{part}`"))
    }

    pub fn delete_part(&mut self, part: &str) -> CmdResult {
        let before = self.model.parts.len();
        self.model.parts.retain(|p| p.name != part);
        if self.model.parts.len() == before {
            Err(ModelerError::NoPart(part.to_string()))
        } else {
            self.model.mark_baked();
            self.commit(&format!("delete `{part}`"));
            Ok(format!("deleted `{part}`"))
        }
    }

    pub fn rename_part(&mut self, part: &str, to: &str) -> CmdResult {
        if self.derived_by_ops(part) {
            self.apply_op(
                Op::Rename {
                    name: to.into(),
                    input: part.into(),
                },
                &format!("rename `{part}` -> `{to}`"),
            )?;
            return Ok(format!("renamed `{part}` -> `{to}`"));
        }
        let p = self
            .model
            .part_mut(part)
            .ok_or_else(|| ModelerError::NoPart(part.to_string()))?;
        p.name = to.to_string();
        self.model.mark_baked();
        self.commit(&format!("rename `{part}` -> `{to}`"));
        Ok(format!("renamed `{part}` -> `{to}`"))
    }

    pub fn set_material(
        &mut self,
        part: &str,
        base_color: Option<[f32; 4]>,
        roughness: Option<f32>,
        metalness: Option<f32>,
    ) -> CmdResult {
        if self.derived_by_ops(part) {
            self.op_material(part, part, base_color, roughness, metalness)?;
            return Ok(format!("material on `{part}` updated"));
        }
        let p = self
            .model
            .part_mut(part)
            .ok_or_else(|| ModelerError::NoPart(part.to_string()))?;
        if let Some(c) = base_color {
            p.material.base_color = c;
        }
        if let Some(r) = roughness {
            p.material.roughness = r;
        }
        if let Some(m) = metalness {
            p.material.metalness = m;
        }
        self.model.mark_baked();
        self.commit(&format!("material on `{part}`"));
        Ok(format!("material on `{part}` updated"))
    }

    pub fn set_uvs(&mut self, part: &str, projection: &str) -> CmdResult {
        let p = self
            .model
            .part_mut(part)
            .ok_or_else(|| ModelerError::NoPart(part.to_string()))?;
        crate::uv::project(p, projection).map_err(|e| e.to_string())?;
        self.model.mark_baked();
        self.commit(&format!("uvs on `{part}`"));
        Ok(format!("uvs on `{part}` set to {projection}"))
    }

    pub fn undo(&mut self) -> CmdResult {
        let (id, mut model) = self.model.history.undo().ok_or("nothing to undo")?;
        model.restore_parts()?;
        model.history = std::mem::take(&mut self.model.history);
        self.model = model;
        Ok(format!("undone to #{id}, model at v{}", self.model.version))
    }

    pub fn redo(&mut self) -> CmdResult {
        let (id, mut model) = self.model.history.redo().ok_or("nothing to redo")?;
        model.restore_parts()?;
        model.history = std::mem::take(&mut self.model.history);
        self.model = model;
        Ok(format!("redone to #{id}, model at v{}", self.model.version))
    }

    /// Jump the cursor anywhere in the history tree. Editing after a jump
    /// branches: the old future stays reachable as a sibling subtree.
    pub fn jump(&mut self, id: usize) -> CmdResult {
        let (_, mut model) = self.model.history.jump(id)?;
        model.restore_parts()?;
        model.history = std::mem::take(&mut self.model.history);
        self.model = model;
        Ok(format!(
            "jumped to #{id}, model at v{} (editing here branches)",
            self.model.version
        ))
    }

    /// Template key of an op: in-place ops share output names, so the kind
    /// is part of the identity.
    pub fn template_key(op: &Op) -> String {
        format!("{}:{}", graph::kind(op), graph::output(op))
    }

    /// The model at history node `id`, or the live model for `None`.
    fn model_at(&self, id: Option<usize>) -> Result<&Model, ModelerError> {
        match id {
            None => Ok(&self.model),
            Some(id) => self
                .model
                .history
                .nodes
                .get(id)
                .map(|n| &n.model)
                .ok_or_else(|| format!("no history node #{id} (see history)").into()),
        }
    }

    /// Ops keyed by (output, kind) — the upsert identity.
    fn op_index(model: &Model) -> Vec<((String, &'static str), &Op)> {
        model
            .ops
            .iter()
            .map(|o| ((graph::output(o).to_string(), graph::kind(o)), o))
            .collect()
    }

    /// Ops in `to` that are new or different from `from`, in `to`'s order.
    fn ops_changed_between(from: &Model, to: &Model) -> Vec<Op> {
        let old = Self::op_index(from);
        Self::op_index(to)
            .into_iter()
            .filter(|(k, op)| {
                old.iter().find(|(ok, _)| ok == k).is_none_or(|(_, o)| {
                    serde_json::to_value(o).ok() != serde_json::to_value(op).ok()
                })
            })
            .map(|(_, op)| op.clone())
            .collect()
    }

    /// What changed in the op graph (and params) from history node `from` to
    /// node `to` (default: the live model): `+` added, `-` removed, `~`
    /// changed, each with the op's full parameters.
    pub fn diff(&self, from: usize, to: Option<usize>) -> CmdResult {
        let (a, b) = (self.model_at(Some(from))?, self.model_at(to)?);
        let (ia, ib) = (Self::op_index(a), Self::op_index(b));
        let mut lines = Vec::new();
        for (k, op) in &ib {
            match ia.iter().find(|(ok, _)| ok == k) {
                None => lines.push(format!("+ {}", Self::op_label(op))),
                Some((_, old)) => {
                    if serde_json::to_value(old).ok() != serde_json::to_value(op).ok() {
                        lines.push(format!(
                            "~ {}\n    was: {}",
                            Self::op_label(op),
                            Self::op_label(old)
                        ));
                    }
                }
            }
        }
        for (k, op) in &ia {
            if !ib.iter().any(|(nk, _)| nk == k) {
                lines.push(format!("- {}", Self::op_label(op)));
            }
        }
        for (k, v) in &b.params {
            match a.params.get(k) {
                None => lines.push(format!("+ param {k}={v}")),
                Some(old) if old != v => lines.push(format!("~ param {k}={v} (was {old})")),
                _ => {}
            }
        }
        for k in a.params.keys().filter(|k| !b.params.contains_key(*k)) {
            lines.push(format!("- param {k}"));
        }
        let to_label = to.map_or("live".to_string(), |t| format!("#{t}"));
        if lines.is_empty() {
            return Ok(format!("#{from} -> {to_label}: no op or param changes"));
        }
        Ok(format!(
            "#{from} -> {to_label}: {} change(s)\n{}",
            lines.len(),
            lines.join("\n")
        ))
    }

    /// Replays ops from history node `from` onto the live model: every op
    /// there that is new or different from the live graph (or just `only`,
    /// by output name), with their parameter templates and any parameters
    /// the live model lacks. The way back after a `jump`: re-apply work from
    /// the abandoned branch without retyping it.
    pub fn cherry_pick(&mut self, from: usize, only: Option<Vec<String>>) -> CmdResult {
        let src = self.model_at(Some(from))?.clone();
        let mut picks = Self::ops_changed_between(&self.model, &src);
        // Params that differ at `from`; they can be picked by name too.
        let param_diff: crate::params::Params = src
            .params
            .iter()
            .filter(|(k, v)| self.model.params.get(*k) != Some(*v))
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        let mut params = crate::params::Params::new();
        if let Some(names) = &only {
            let missing: Vec<&String> = names
                .iter()
                .filter(|n| {
                    !picks.iter().any(|o| graph::output(o) == n.as_str())
                        && !param_diff.contains_key(n.as_str())
                })
                .collect();
            if !missing.is_empty() {
                let mut available: Vec<String> =
                    picks.iter().map(|o| graph::output(o).to_string()).collect();
                available.sort();
                available.dedup();
                available.extend(param_diff.keys().map(|k| format!("param {k}")));
                return Err(format!(
                    "node #{from} has no new/changed op or param named {} (available: {})",
                    missing
                        .iter()
                        .map(|n| format!("`{n}`"))
                        .collect::<Vec<_>>()
                        .join(", "),
                    if available.is_empty() {
                        "none".to_string()
                    } else {
                        available.join(", ")
                    }
                )
                .into());
            }
            picks.retain(|o| names.iter().any(|n| n == graph::output(o)));
            for n in names {
                if let Some(v) = param_diff.get(n) {
                    params.insert(n.clone(), *v);
                }
            }
        } else {
            params = param_diff.clone();
        }
        // A picked op brings the param values it was derived with, so the
        // live graph stays consistent with its params.
        let templates: Vec<Option<serde_json::Value>> = picks
            .iter()
            .map(|op| src.templates.get(&Self::template_key(op)).cloned())
            .collect();
        for t in templates.iter().flatten() {
            for r in crate::params::refs(t) {
                if let Some(v) = src.params.get(&r) {
                    params.insert(r, *v);
                }
            }
        }
        if picks.is_empty() && params.is_empty() {
            return Ok(format!(
                "nothing to pick: node #{from} has no ops or params that differ from the live graph"
            ));
        }
        let mut out = Vec::new();
        self.batch(|s| {
            if !params.is_empty() {
                out.push(s.apply_values(Some(params.clone()), Vec::new())?);
            }
            if !picks.is_empty() {
                let items = picks
                    .into_iter()
                    .zip(templates)
                    .map(|(op, t)| (BatchItem::Op(op), t))
                    .collect();
                out.push(s.apply_items(items, false)?);
            }
            Ok(())
        })?;
        Ok(format!("cherry-picked from #{from}:\n{}", out.join("\n")))
    }

    /// Prunes old history, keeping the newest `keep` snapshots, the cursor,
    /// the root and every branch point. Ids are renumbered.
    pub fn history_prune(&mut self, keep: usize) -> CmdResult {
        let removed = self.model.history.prune(keep);
        Ok(format!(
            "pruned {removed} history node(s); {} remain (ids renumbered)\n{}",
            self.model.history.nodes.len(),
            self.model.history.describe()
        ))
    }

    /// The history tree, one line per snapshot: the newest `limit` (default
    /// 30) plus the cursor line, with a count of the elided older nodes.
    pub fn history(&self, limit: Option<usize>) -> String {
        let all = self.model.history.describe();
        let lines: Vec<&str> = all.lines().collect();
        let limit = limit.unwrap_or(30).max(1);
        if lines.len() <= limit {
            return all;
        }
        let tail_start = lines.len() - limit;
        let mut out = vec![format!(
            "{} history nodes; showing the newest {limit} (history limit=N for more)",
            lines.len()
        )];
        if let Some(cur) = lines[..tail_start].iter().find(|l| l.ends_with("<-")) {
            out.push(format!("{cur}  (cursor)"));
            out.push("…".into());
        }
        out.extend(lines[tail_start..].iter().map(|l| l.to_string()));
        out.join("\n")
    }

    /// Comment on a part (optionally a specific face or vertex). Face, vertex and
    /// position are all optional; when all three are omitted and the human has
    /// clicked a spot in the live view, that pending pin is used instead. Annotations
    /// are not geometry: they never bump the model version but do persist in prefabs.
    pub fn add_annotation(
        &mut self,
        part: &str,
        text: &str,
        face: Option<u32>,
        vertex: Option<u32>,
        position: Option<[f32; 3]>,
    ) -> CmdResult {
        let (part, face, vertex, position) = if face.is_none()
            && vertex.is_none()
            && position.is_none()
            && let Some(pin) = self.pending_pin.take()
        {
            (pin.part, pin.face, pin.vertex, pin.position)
        } else {
            let centroid = self
                .model
                .part_centroid(part)
                .ok_or_else(|| ModelerError::NoPart(part.to_string()))?;
            (part.to_string(), face, vertex, position.unwrap_or(centroid))
        };
        let id = self.model.next_annotation_id;
        self.model.next_annotation_id += 1;
        let annotation = Annotation {
            id,
            part: part.clone(),
            face,
            vertex,
            position,
            text: text.to_string(),
        };
        self.model.annotations.push(annotation);
        Ok(format!(
            "annotation #{id} on `{part}`: '{text}'{}",
            face.map(|f| format!(" @face {f}")).unwrap_or_default()
        ))
    }

    /// Every annotation, one line each, so the AI can read the human's comments.
    pub fn list_annotations(&self) -> String {
        if self.model.annotations.is_empty() {
            return "no annotations yet".into();
        }
        let mut out = format!("{} annotations\n", self.model.annotations.len());
        for a in &self.model.annotations {
            out += &format!(
                "#{} `{}` '{}'{}{}\n",
                a.id,
                a.part,
                a.text,
                a.face.map(|f| format!(" @face {f}")).unwrap_or_default(),
                a.vertex
                    .map(|v| format!(" @vertex {v}"))
                    .unwrap_or_default(),
            );
        }
        out
    }

    /// Remove one annotation by id.
    pub fn delete_annotation(&mut self, id: u32) -> CmdResult {
        let before = self.model.annotations.len();
        self.model.annotations.retain(|a| a.id != id);
        if self.model.annotations.len() == before {
            Err(format!("no annotation #{id}").into())
        } else {
            Ok(format!("deleted annotation #{id}"))
        }
    }

    pub fn summary(&self) -> String {
        let bounds = self
            .model
            .bounds()
            .map(|(min, max)| format!("bounds {min:?}..{max:?}"))
            .unwrap_or_else(|| "empty".into());
        let mut s = format!(
            "model `{}` v{}: {} parts, {bounds}\n",
            self.model.name,
            self.model.version,
            self.model.parts.len()
        );
        if !self.model.ops.is_empty() {
            s += &(graph::describe(&self.model.ops) + "\n");
        }
        for p in &self.model.parts {
            s += &format!(
                "- `{}`{}: {} verts {} tris, mat {:?}, at {:?}\n",
                p.name,
                if p.visible { "" } else { " (hidden)" },
                p.verts.len(),
                p.tri_count(),
                p.material.base_color,
                p.translation
            );
        }
        if self.model.annotations.is_empty() {
            s += "annotations: none\n";
        } else {
            s += &format!("annotations ({})\n", self.model.annotations.len());
            for a in &self.model.annotations {
                s += &format!(
                    "- #{} `{}` '{}'{}{}\n",
                    a.id,
                    a.part,
                    a.text,
                    a.face.map(|f| format!(" @face {f}")).unwrap_or_default(),
                    a.vertex
                        .map(|v| format!(" @vertex {v}"))
                        .unwrap_or_default(),
                );
            }
        }
        if self.pending_pin.is_some() {
            s += "a pin is waiting in the live view: use add_annotation without face/vertex/position to comment on it\n";
        }
        let lints = self.lints();
        if !lints.is_empty() {
            s += &format!("warnings ({}):\n", lints.len());
            for l in &lints {
                s += &format!("- {l}\n");
            }
        }
        s += "tip: `validate` checks manifoldness and winding per part\n";
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_with_part() -> Session {
        let mut s = Session::default();
        s.add_primitive(
            crate::PrimitiveKind::Box,
            [1.0, 1.0, 1.0],
            [0.0, 0.0, 0.0],
            "leg",
        )
        .unwrap_or_else(|e| panic!("{e}"));
        s
    }

    #[test]
    fn unknown_part_errs() {
        let mut s = session_with_part();
        assert!(s.add_annotation("nope", "hi", None, None, None).is_err());
    }

    #[test]
    fn ids_are_monotonic() {
        let mut s = session_with_part();
        s.add_annotation("leg", "a", None, None, None)
            .unwrap_or_else(|e| panic!("{e}"));
        s.add_annotation("leg", "b", None, None, None)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.model.annotations[0].id, 0);
        assert_eq!(s.model.annotations[1].id, 1);
        assert!(s.model.annotation_at(1).is_some());
        assert!(s.model.annotation_at(9).is_none());
    }

    #[test]
    fn list_delete_round_trip() {
        let mut s = session_with_part();
        assert_eq!(s.list_annotations(), "no annotations yet");
        s.add_annotation("leg", "make this thicker", Some(12), None, None)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(
            s.list_annotations()
                .contains("#0 `leg` 'make this thicker' @face 12")
        );
        s.delete_annotation(0).unwrap_or_else(|e| panic!("{e}"));
        assert!(s.delete_annotation(0).is_err());
        assert_eq!(s.list_annotations(), "no annotations yet");
    }

    #[test]
    fn pending_pin_is_auto_consumed() {
        let mut s = session_with_part();
        s.pending_pin = Some(PendingPin {
            part: "leg".into(),
            face: Some(3),
            vertex: Some(7),
            position: [0.5, 0.0, 0.5],
        });
        s.add_annotation("leg", "here", None, None, None)
            .unwrap_or_else(|e| panic!("{e}"));
        let a = &s.model.annotations[0];
        assert_eq!(a.face, Some(3));
        assert_eq!(a.vertex, Some(7));
        assert_eq!(a.position, [0.5, 0.0, 0.5]);
        assert!(s.pending_pin.is_none());
    }

    #[test]
    fn annotations_do_not_bump_version_but_persist_via_serde() {
        let mut s = session_with_part();
        let v = s.model.version;
        s.add_annotation("leg", "note", None, None, Some([0.0, 1.0, 0.0]))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.model.version, v);

        // An old prefab without annotations still loads.
        let old = serde_json::to_string(&Model::named("old")).unwrap_or_else(|e| panic!("{e}"));
        let parsed: Model = serde_json::from_str(&old).unwrap_or_else(|e| panic!("{e}"));
        assert!(parsed.annotations.is_empty());

        // New prefabs carry their annotations.
        let json = serde_json::to_string(&s.model).unwrap_or_else(|e| panic!("{e}"));
        let parsed: Model = serde_json::from_str(&json).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(parsed.annotations[0].text, "note");
    }

    #[test]
    fn summary_includes_annotation_text() {
        let mut s = session_with_part();
        s.add_annotation("leg", "wobbly", None, None, None)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(s.summary().contains("wobbly"));
    }

    fn undo(s: &mut Session) {
        s.undo().unwrap_or_else(|e| panic!("{e}"));
    }

    #[test]
    fn history_tree_undoes_and_redoes_linear_edits() {
        let mut s = session_with_part();
        s.rename_part("leg", "leg2")
            .unwrap_or_else(|e| panic!("{e}"));
        undo(&mut s);
        assert_eq!(s.model.parts[0].name, "leg");
        s.redo().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.model.parts[0].name, "leg2");
        assert!(s.history(None).contains('#'));
    }

    #[test]
    fn editing_after_undo_branches_instead_of_discarding() {
        let mut s = session_with_part();
        s.rename_part("leg", "b").unwrap_or_else(|e| panic!("{e}"));
        undo(&mut s);
        // A new edit while back at the older state: the rename stays reachable.
        s.rename_part("leg", "c").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.model.parts[0].name, "c");
        assert!(s.history(None).contains("`rename `leg` -> `b`"));
        // Jump to the abandoned branch's snapshot and the name comes back.
        let id = s
            .model
            .history
            .nodes
            .iter()
            .position(|n| n.label.contains("-> `b`"))
            .unwrap_or_else(|| panic!("branch snapshot missing"));
        s.jump(id).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.model.parts[0].name, "b");
    }

    #[test]
    fn procedural_snapshots_serialize_without_parts_but_with_the_tree() {
        use crate::History;
        let mut s = Session::default();
        s.op_primitive(
            "leg",
            crate::PrimitiveKind::Box,
            [1.0, 1.0, 1.0],
            [0.0, 0.5, 0.0],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        s.op_primitive(
            "ball",
            crate::PrimitiveKind::Sphere,
            [0.5, 0.0, 0.0],
            [0.0, 2.0, 0.0],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let json = serde_json::to_string(&s.model.history).unwrap_or_else(|e| panic!("{e}"));
        assert!(json.contains("ball"), "{json}");
        let back: History = serde_json::from_str(&json).unwrap_or_else(|e| panic!("{e}"));
        // The first snapshot is a baked empty model; the second is procedural
        // and carries ops only, re-deriving both parts.
        assert!(back.nodes[0].model.parts.is_empty());
        let mut model = back.nodes[1].model.clone();
        assert!(model.parts.is_empty());
        assert!(model.refresh().is_ok());
        assert_eq!(model.parts.len(), 2);
    }

    #[test]
    fn undo_at_the_root_errs() {
        let mut s = Session::default();
        assert!(s.undo().is_err());
        assert!(s.redo().is_err());
        assert!(s.jump(42).is_err());
    }

    #[test]
    fn ops_derive_parts_and_tweak_in_place() {
        let mut s = Session::default();
        s.op_primitive(
            "stem",
            crate::PrimitiveKind::Cylinder,
            [0.05, 1.0, 0.0],
            [0.0, 0.5, 0.0],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        s.op_primitive(
            "petal",
            crate::PrimitiveKind::Sphere,
            [0.2, 0.0, 0.0],
            [0.0, 1.0, 0.3],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        s.op_radial_array(
            "ring",
            "petal",
            6,
            [0.0, 1.0, 0.0],
            360.0,
            0.0,
            crate::Axis::Y,
            false,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(s.model.procedural());
        // stem + petal + 6 ring copies
        assert_eq!(s.model.parts.len(), 8);
        assert!(s.summary().contains("ops (3)"));

        // Re-issuing the radial op with another count is the tweak path.
        let v = s.model.version;
        s.op_radial_array(
            "ring",
            "petal",
            8,
            [0.0, 1.0, 0.0],
            360.0,
            0.0,
            crate::Axis::Y,
            false,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.model.parts.len(), 10);
        assert_eq!(s.model.ops.len(), 3);
        assert!(s.model.version > v);
    }

    #[test]
    fn bad_op_is_rejected_and_leaves_graph_intact() {
        let mut s = Session::default();
        s.op_primitive(
            "a",
            crate::PrimitiveKind::Box,
            [1.0, 1.0, 1.0],
            [0.0, 0.0, 0.0],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let v = s.model.version;
        assert!(
            s.op_boolean("cut", crate::BoolOp::Difference, "a", "ghost")
                .is_err()
        );
        assert_eq!(s.model.ops.len(), 1);
        assert_eq!(s.model.version, v);
        assert_eq!(s.model.parts.len(), 1);
    }

    #[test]
    fn op_delete_rederives() {
        let mut s = Session::default();
        s.op_primitive(
            "a",
            crate::PrimitiveKind::Box,
            [1.0, 1.0, 1.0],
            [0.0, 0.0, 0.0],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        s.op_primitive(
            "b",
            crate::PrimitiveKind::Box,
            [1.0, 1.0, 1.0],
            [1.0, 0.0, 0.0],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        s.op_delete("a").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.model.parts.len(), 1);
        assert!(s.op_delete("a").is_err());
    }

    fn box_op(name: &str, translation: [f32; 3]) -> Op {
        Op::Primitive {
            name: name.into(),
            kind: crate::PrimitiveKind::Box,
            size: [1.0, 1.0, 1.0],
            translation,
            rotation: None,
        }
    }

    #[test]
    fn new_model_is_undoable() {
        let mut s = session_with_part();
        let old_name = s.model.name.clone();
        s.new_model("second").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.model.name, "second");
        assert!(s.model.parts.is_empty());
        // One undo brings the old model back intact.
        s.undo().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.model.name, old_name);
        assert_eq!(s.model.parts.len(), 1);
    }

    #[test]
    fn apply_ops_batch_applies_in_order() {
        let mut s = Session::default();
        let ops = vec![
            box_op("a", [0.0, 0.5, 0.0]),
            box_op("b", [3.0, 0.5, 0.0]),
            Op::Material {
                name: "a".into(),
                input: "a".into(),
                base_color: Some([1.0, 0.0, 0.0, 1.0]),
                roughness: None,
                metalness: None,
            },
        ];
        let out = s.apply_ops(ops).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.model.parts.len(), 2);
        assert!(out.contains("material `a` <- `a`"));
        assert!(out.contains("model `model`"));
    }

    #[test]
    fn apply_ops_stops_at_first_error_and_keeps_earlier_ops() {
        let mut s = Session::default();
        let ops = vec![
            box_op("a", [0.0, 0.5, 0.0]),
            Op::Boolean {
                name: "c".into(),
                op: crate::BoolOp::Union,
                a: "a".into(),
                b: "ghost".into(),
            },
            box_op("b", [3.0, 0.5, 0.0]),
        ];
        let Err(err) = s.apply_ops(ops) else {
            panic!("expected the batch to fail")
        };
        assert!(
            err.to_string().contains("batch item #1"),
            "error names the failing index: {err}"
        );
        assert_eq!(s.model.parts.len(), 1, "op #0 stayed applied");
        assert!(!s.model.ops.iter().any(|o| crate::graph::output(o) == "b"));
    }

    #[test]
    fn hide_and_show_part() {
        let mut s = session_with_part();
        s.set_visible("leg", false)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(!s.model.parts[0].visible);
        assert!(s.summary().contains("(hidden)"));
        s.set_visible("leg", true).unwrap_or_else(|e| panic!("{e}"));
        assert!(s.model.parts[0].visible);
        assert!(s.set_visible("nope", false).is_err());
    }

    #[test]
    fn hidden_part_is_excluded_from_bounds() {
        let mut s = session_with_part();
        let Some((amin, amax)) = s.model.bounds() else {
            panic!("bounds should exist")
        };
        assert!((amin.y + 0.5).abs() < 1e-5 && (amax.y - 0.5).abs() < 1e-5);
        s.set_visible("leg", false)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(s.model.bounds().is_none());
    }

    #[test]
    fn op_get_returns_full_parameters() {
        let mut s = Session::default();
        s.op_primitive(
            "leg",
            crate::PrimitiveKind::Cylinder,
            [0.1, 0.5, 0.0],
            [0.2, 0.25, 0.0],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let d = s.describe_op("leg").unwrap_or_else(|e| panic!("{e}"));
        assert!(d.contains("cylinder"), "{d}");
        assert!(d.contains("size=[0.1, 0.5, 0.0]"), "{d}");
        assert!(d.contains("at=[0.2, 0.25, 0.0]"), "{d}");
        assert!(s.describe_op("nope").is_err());
    }

    #[test]
    fn copies_inherit_a_material_placed_after_them() {
        let mut s = Session::default();
        s.op_primitive(
            "arm",
            crate::PrimitiveKind::Box,
            [0.1, 0.5, 0.1],
            [0.4, 1.0, 0.0],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        s.op_mirror("arm_r", "arm", crate::Axis::X, [0.0, 0.0, 0.0])
            .unwrap_or_else(|e| panic!("{e}"));
        s.op_linear_array("arms", "arm", 3, [0.0, 0.0, 1.0])
            .unwrap_or_else(|e| panic!("{e}"));
        // Material op written after the copies: lineage still colors them.
        s.op_material("arm", "arm", Some([1.0, 0.0, 0.0, 1.0]), None, None)
            .unwrap_or_else(|e| panic!("{e}"));
        for n in ["arm", "arm_r", "arms_1", "arms_2"] {
            let p = s.model.part(n).unwrap_or_else(|| panic!("{n}"));
            assert_eq!(p.material.base_color, [1.0, 0.0, 0.0, 1.0], "{n}");
        }
        assert!(!s.summary().contains("default material"));
    }

    #[test]
    fn lint_full_ring_radial_duplicate() {
        let mut s = Session::default();
        s.op_primitive(
            "post",
            crate::PrimitiveKind::Cylinder,
            [0.05, 0.5, 0.0],
            [0.6, 0.25, 0.0],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        s.op_radial_array(
            "posts",
            "post",
            6,
            [0.0, 0.25, 0.0],
            360.0,
            0.0,
            crate::Axis::Y,
            false,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(
            s.lints()
                .iter()
                .any(|l| l.contains("source and copy 0 overlap")),
            "lints: {:?}",
            s.lints()
        );
        // The even variant (count-1 copies starting one step in) is clean.
        let mut s2 = Session::default();
        s2.op_primitive(
            "post",
            crate::PrimitiveKind::Cylinder,
            [0.05, 0.5, 0.0],
            [0.6, 0.25, 0.0],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        s2.op_radial_array(
            "posts",
            "post",
            5,
            [0.0, 0.25, 0.0],
            360.0,
            60.0,
            crate::Axis::Y,
            false,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(!s2.lints().iter().any(|l| l.contains("overlap")));
    }

    #[test]
    fn lint_default_material_part() {
        let s = session_with_part();
        assert!(
            s.lints()
                .iter()
                .any(|l| l.contains("`leg` still has the default material")),
            "lints: {:?}",
            s.lints()
        );
    }

    #[test]
    fn lint_coincident_bounds_zfight() {
        let mut s = Session::default();
        let ops = vec![box_op("a", [0.0, 0.5, 0.0]), box_op("b", [0.0, 0.5, 0.0])];
        s.apply_ops(ops).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            s.lints().iter().any(|l| l.contains("identical bounds")),
            "lints: {:?}",
            s.lints()
        );
    }

    #[test]
    fn lint_boolean_volume_sanity() {
        // A difference whose output kept the input's full volume: the cutter
        // misses. Built by hand (ops + matching parts) so the volumes are
        // exact — a real difference with an overlapping cutter shrinks.
        let mut s = Session::default();
        let mut a = Part::new("a");
        a.verts = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 1.0],
            [1.0, 1.0, 1.0],
            [0.0, 1.0, 1.0],
        ];
        a.indices = vec![
            0, 2, 1, 0, 3, 2, 4, 5, 6, 4, 6, 7, 0, 1, 5, 0, 5, 4, 1, 2, 6, 1, 6, 5, 2, 3, 7, 2, 7,
            6, 3, 0, 4, 3, 4, 7,
        ];
        crate::uv::ensure_normals(&mut a);
        let mut hollow = a.clone();
        hollow.name = "hollow".into();
        let mut cutter = Part::new("b");
        cutter.verts = vec![
            [0.1, 0.1, 0.1],
            [0.9, 0.1, 0.1],
            [0.9, 0.9, 0.1],
            [0.1, 0.9, 0.1],
            [0.1, 0.1, 0.9],
            [0.9, 0.1, 0.9],
            [0.9, 0.9, 0.9],
            [0.1, 0.9, 0.9],
        ];
        cutter.indices = a.indices.clone();
        crate::uv::ensure_normals(&mut cutter);
        s.model.parts = vec![a.into(), cutter.into(), hollow.into()];
        s.model.ops = vec![
            box_op("a", [0.0, 0.0, 0.0]),
            box_op("b", [0.0, 0.0, 0.0]),
            Op::Boolean {
                name: "hollow".into(),
                op: crate::BoolOp::Difference,
                a: "a".into(),
                b: "b".into(),
            },
        ];
        s.model.version += 1;
        let lints = s.lints();
        assert!(
            lints
                .iter()
                .any(|l| l.contains("difference `hollow` left `a` at its original volume")),
            "lints: {:?}",
            lints
        );

        // A union whose output volume exactly equals a + b: the inputs
        // never overlapped. Two disjoint slabs, hand-welded union mesh.
        let mut s2 = Session::default();
        let mut slab_a = Part::new("sa");
        slab_a.verts = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 1.0],
            [1.0, 1.0, 1.0],
            [0.0, 1.0, 1.0],
        ];
        slab_a.indices = vec![
            0, 2, 1, 0, 3, 2, 4, 5, 6, 4, 6, 7, 0, 1, 5, 0, 5, 4, 1, 2, 6, 1, 6, 5, 2, 3, 7, 2, 7,
            6, 3, 0, 4, 3, 4, 7,
        ];
        let mut slab_b = slab_a.clone();
        slab_b.name = "sb".into();
        for v in &mut slab_b.verts {
            v[0] += 5.0;
        }
        let mut joined = Part::new("joined");
        joined.verts = [slab_a.verts.as_slice(), slab_b.verts.as_slice()].concat();
        joined.indices = slab_a.indices.clone();
        let off = slab_a.verts.len() as u32;
        joined
            .indices
            .extend(slab_b.indices.iter().map(|i| i + off));
        crate::uv::ensure_normals(&mut slab_a);
        crate::uv::ensure_normals(&mut slab_b);
        crate::uv::ensure_normals(&mut joined);
        s2.model.parts = vec![slab_a.into(), slab_b.into(), joined.into()];
        s2.model.ops = vec![
            box_op("sa", [0.0, 0.5, 0.0]),
            box_op("sb", [5.5, 0.5, 0.0]),
            Op::Boolean {
                name: "joined".into(),
                op: crate::BoolOp::Union,
                a: "sa".into(),
                b: "sb".into(),
            },
        ];
        s2.model.version += 1;
        let lints = s2.lints();
        assert!(
            lints
                .iter()
                .any(|l| l.contains("the inputs have no overlap")),
            "lints: {:?}",
            lints
        );
        // Unioning disjoint cutters is intentional: once the union is cut
        // out of something, the no-overlap warning stays quiet.
        s2.model.ops.push(Op::Boolean {
            name: "cut".into(),
            op: crate::BoolOp::Difference,
            a: "sa".into(),
            b: "joined".into(),
        });
        assert!(
            !s2.lints().iter().any(|l| l.contains("no overlap")),
            "cutter union still warned"
        );
    }

    #[test]
    fn validate_flags_inverted_mesh_and_passes_primitives() {
        let mut s = Session::default();
        s.op_primitive(
            "box",
            crate::PrimitiveKind::Box,
            [1.0, 1.0, 1.0],
            [0.0, 0.5, 0.0],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(s.validate().contains("all parts: manifold, outward-wound"));

        // An inward-wound closed tetrahedron flips the signed volume negative.
        let mut p = Part::new("tetra");
        p.verts = vec![
            [1.0, 1.0, 1.0],
            [-1.0, -1.0, 1.0],
            [-1.0, 1.0, -1.0],
            [1.0, -1.0, -1.0],
        ];
        p.indices = vec![0, 1, 2, 0, 2, 3, 0, 3, 1, 1, 3, 2];
        crate::uv::ensure_normals(&mut p);
        s.model.parts.push(p.into());
        s.model.version += 1;
        let v = s.validate();
        assert!(v.contains("`tetra`: inverted winding"), "{v}");
        assert!(!v.contains("`box`"), "the good part is not reported: {v}");
    }

    #[test]
    fn check_rebuild_detects_stale_cache() {
        let mut s = Session::default();
        s.op_primitive(
            "box",
            crate::PrimitiveKind::Box,
            [1.0, 1.0, 1.0],
            [0.0, 0.5, 0.0],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        s.check_rebuild().unwrap_or_else(|e| panic!("{e}"));
        // Simulate drift: nudge the cached part without touching the ops.
        std::sync::Arc::make_mut(&mut s.model.parts[0]).translation = [9.0, 0.5, 0.0];
        let Err(err) = s.check_rebuild() else {
            panic!("stale cache should be reported")
        };
        assert!(err.to_string().contains("stale"), "{err}");
    }

    #[test]
    fn op_primitive_rotation_is_absolute() {
        let mut s = Session::default();
        let h = std::f32::consts::FRAC_1_SQRT_2;
        let z90 = crate::quat_of([0.0, 0.0, h, h]).to_array();
        s.op_primitive(
            "beam",
            crate::PrimitiveKind::Box,
            [2.0, 0.2, 0.2],
            [0.0, 0.1, 0.0],
            Some(z90),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.model.parts[0].rotation, z90);
        // And the upsert path accepts a changed rotation.
        s.op_primitive(
            "beam",
            crate::PrimitiveKind::Box,
            [2.0, 0.2, 0.2],
            [0.0, 0.1, 0.0],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.model.parts[0].rotation, [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn expect_parts_reports_missing_and_extra() {
        let mut s = Session::default();
        s.op_primitive(
            "a",
            crate::PrimitiveKind::Box,
            [1.0, 1.0, 1.0],
            [0.0, 0.5, 0.0],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        s.expect_parts(&["a".into()])
            .unwrap_or_else(|e| panic!("{e}"));
        let Err(err) = s.expect_parts(&["a".into(), "b".into()]) else {
            panic!("missing part should be reported")
        };
        assert!(err.to_string().contains("missing"), "{err}");
        let Err(err) = s.expect_parts(&[]) else {
            panic!("extra part should be reported")
        };
        assert!(err.to_string().contains("unexpected"), "{err}");
    }

    #[test]
    fn expect_bounds_checks_axes_with_tolerance() {
        let mut s = Session::default();
        s.op_primitive(
            "box",
            crate::PrimitiveKind::Box,
            [1.0, 1.0, 1.0],
            [0.0, 0.5, 0.0],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        // Sits on the ground, 1 unit tall, centered in x (spans x -0.5..0.5).
        s.expect_bounds(
            [Some(-0.5), Some(0.0), None],
            [Some(0.5), Some(1.0), None],
            0.01,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        // Off-by-more-than-tolerance expectations fail, naming the axis:
        // demanding min y 0.5 on a ground-sitting box = "the model floats".
        let Err(err) = s.expect_bounds([None, Some(0.5), None], [None, Some(1.0), None], 0.01)
        else {
            panic!("floating model should be reported")
        };
        assert!(err.to_string().contains("min y"), "{err}");
        // Skipped axes (None) never fail.
        s.expect_bounds([None, None, None], [None, None, None], 0.01)
            .unwrap_or_else(|e| panic!("{e}"));
    }

    #[test]
    fn colliding_output_names_are_rejected() {
        let mut s = Session::default();
        s.op_primitive("p", crate::PrimitiveKind::Box, [1.0; 3], [0.0; 3], None)
            .unwrap_or_else(|e| panic!("{e}"));
        s.op_radial_array("ring", "p", 3, [0.0; 3], 360.0, 0.0, crate::Axis::Y, false)
            .unwrap_or_else(|e| panic!("{e}"));
        let err = s.op_primitive(
            "ring_0",
            crate::PrimitiveKind::Box,
            [1.0; 3],
            [0.0; 3],
            None,
        );
        assert!(err.is_err(), "ring_0 is already a radial output");
        assert_eq!(s.model.ops.len(), 2);
    }

    #[test]
    fn op_delete_reports_array_dependents() {
        let mut s = Session::default();
        s.op_primitive("p", crate::PrimitiveKind::Box, [1.0; 3], [0.0; 3], None)
            .unwrap_or_else(|e| panic!("{e}"));
        s.op_radial_array("ring", "p", 3, [0.0; 3], 360.0, 0.0, crate::Axis::Y, false)
            .unwrap_or_else(|e| panic!("{e}"));
        s.op_transform("moved", "ring_1", Some([0.0, 3.0, 0.0]), None, None)
            .unwrap_or_else(|e| panic!("{e}"));
        let err = s
            .op_delete("ring")
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(err.to_string().contains("moved"), "{err}");
        assert_eq!(s.model.ops.len(), 3);
    }

    #[test]
    fn undo_keeps_baked_hide_on_op_models() {
        let mut s = Session::default();
        s.op_primitive("p", crate::PrimitiveKind::Box, [1.0; 3], [0.0; 3], None)
            .unwrap_or_else(|e| panic!("{e}"));
        s.set_visible("p", false).unwrap_or_else(|e| panic!("{e}"));
        s.op_primitive(
            "q",
            crate::PrimitiveKind::Box,
            [1.0; 3],
            [3.0, 0.0, 0.0],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        s.undo().unwrap_or_else(|e| panic!("{e}"));
        assert!(
            s.model.part("p").is_some_and(|p| !p.visible),
            "hide lost on undo"
        );
    }

    fn op_session() -> Session {
        let mut s = Session::default();
        s.op_primitive("p", crate::PrimitiveKind::Box, [1.0; 3], [0.0; 3], None)
            .unwrap_or_else(|e| panic!("{e}"));
        s.op_radial_array("ring", "p", 3, [0.0; 3], 360.0, 0.0, crate::Axis::Y, false)
            .unwrap_or_else(|e| panic!("{e}"));
        s
    }

    #[test]
    fn hide_on_op_models_is_a_graph_op_that_survives_tweaks() {
        let mut s = op_session();
        s.set_visible("p", false).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            s.model
                .ops
                .iter()
                .any(|o| matches!(o, Op::Visibility { .. }))
        );
        // Tweaking an upstream op re-derives, but the hide is part of the graph.
        s.op_primitive("p", crate::PrimitiveKind::Box, [2.0; 3], [0.0; 3], None)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(s.model.part("p").is_some_and(|p| !p.visible));
        // Copies made from the hidden template are visible.
        assert!(s.model.part("ring_1").is_some_and(|p| p.visible));
        // Re-issuing replaces (no stacking), and show works.
        s.set_visible("p", true).unwrap_or_else(|e| panic!("{e}"));
        s.set_visible("p", false).unwrap_or_else(|e| panic!("{e}"));
        s.set_visible("p", true).unwrap_or_else(|e| panic!("{e}"));
        let n = s
            .model
            .ops
            .iter()
            .filter(|o| matches!(o, Op::Visibility { .. }))
            .count();
        assert_eq!(n, 1);
        assert!(s.model.part("p").is_some_and(|p| p.visible));
    }

    #[test]
    fn rename_on_op_models_survives_tweaks_and_keeps_readers_working() {
        let mut s = op_session();
        s.rename_part("p", "seed").unwrap_or_else(|e| panic!("{e}"));
        assert!(s.model.part("seed").is_some());
        assert!(s.model.part("p").is_none());
        // The radial op still reads the old name and runs before the rename.
        assert!(s.model.part("ring_0").is_some());
        s.op_primitive("p", crate::PrimitiveKind::Box, [3.0; 3], [0.0; 3], None)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(s.model.part("seed").is_some(), "rename lost after a tweak");
        s.check_rebuild().unwrap_or_else(|e| panic!("{e}"));
        // Undo returns to the pre-rename names.
        s.undo().unwrap_or_else(|e| panic!("{e}"));
        s.undo().unwrap_or_else(|e| panic!("{e}"));
        assert!(s.model.part("p").is_some());
    }

    #[test]
    fn material_and_transform_on_op_models_are_graph_ops() {
        let mut s = op_session();
        s.set_material("p", Some([0.0, 1.0, 0.0, 1.0]), None, None)
            .unwrap_or_else(|e| panic!("{e}"));
        s.transform_part("p", Some([0.0, 5.0, 0.0]), None, None)
            .unwrap_or_else(|e| panic!("{e}"));
        s.op_primitive("p", crate::PrimitiveKind::Box, [2.0; 3], [0.0; 3], None)
            .unwrap_or_else(|e| panic!("{e}"));
        let p = s.model.part("p").unwrap_or_else(|| panic!("no p"));
        assert_eq!(p.material.base_color, [0.0, 1.0, 0.0, 1.0]);
        assert_eq!(p.translation, [0.0, 5.0, 0.0]);
    }

    #[test]
    fn op_snapshots_store_no_parts_and_undo_rederives() {
        let mut s = op_session();
        s.op_primitive(
            "q",
            crate::PrimitiveKind::Box,
            [1.0; 3],
            [4.0, 0.0, 0.0],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(
            s.model
                .history
                .nodes
                .iter()
                .all(|n| n.model.parts.is_empty())
        );
        let live = s.model.parts.len();
        s.undo().unwrap_or_else(|e| panic!("{e}"));
        assert!(s.model.part("q").is_none());
        assert!(!s.model.parts.is_empty());
        s.redo().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.model.parts.len(), live);
    }

    #[test]
    fn history_prune_keeps_newest_branch_points_and_cursor() {
        let mut s = op_session();
        for i in 0..10 {
            s.op_primitive(
                "q",
                crate::PrimitiveKind::Box,
                [1.0 + i as f32; 3],
                [0.0; 3],
                None,
            )
            .unwrap_or_else(|e| panic!("{e}"));
        }
        let total = s.model.history.nodes.len();
        let out = s.history_prune(3).unwrap_or_else(|e| panic!("{e}"));
        let h = &s.model.history;
        assert!(h.nodes.len() < total, "{out}");
        assert_eq!(h.current, Some(h.nodes.len() - 1));
        // Tree stays consistent: every parent link has the matching child link.
        for (i, n) in h.nodes.iter().enumerate() {
            if let Some(p) = n.parent {
                assert!(h.nodes[p].children.contains(&i));
            }
        }
        // Still navigable.
        s.undo().unwrap_or_else(|e| panic!("{e}"));
        s.redo().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.history_prune(1000).map(|_| ()), Ok(()));
    }

    #[test]
    fn errors_are_typed_so_tests_can_match_variants() {
        use crate::ModelerError;
        let mut s = op_session();
        assert_eq!(
            s.set_uvs("ghost", "box"),
            Err(ModelerError::NoPart("ghost".into()))
        );
        // A dangling input surfaces as MissingInput, not a stringly error.
        let err = s.op_transform("t", "ghost", None, None, None).err();
        assert_eq!(err, Some(ModelerError::MissingInput("ghost".into())));
        assert!(matches!(
            crate::graph::evaluate(&[
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
            ]),
            Err(ModelerError::Cycle(_))
        ));
    }

    #[test]
    fn baked_history_snapshots_share_unchanged_parts() {
        let mut s = session_with_part();
        s.add_primitive(
            crate::PrimitiveKind::Box,
            [1.0; 3],
            [5.0, 0.0, 0.0],
            "other",
        )
        .unwrap_or_else(|e| panic!("{e}"));
        s.transform_part("other", Some([6.0, 0.0, 0.0]), None, None)
            .unwrap_or_else(|e| panic!("{e}"));
        let nodes = &s.model.history.nodes;
        let (a, b) = (&nodes[nodes.len() - 2].model, &nodes[nodes.len() - 1].model);
        // `leg` was untouched by the last edit: both snapshots point at one mesh.
        assert!(std::sync::Arc::ptr_eq(&a.parts[0], &b.parts[0]));
        assert!(!std::sync::Arc::ptr_eq(&a.parts[1], &b.parts[1]));
    }

    #[test]
    fn radial_include_source_spaces_count_items_evenly_without_overlap() {
        let mut s = Session::default();
        s.op_primitive(
            "blade",
            crate::PrimitiveKind::Box,
            [0.1, 1.0, 0.1],
            [0.0, 1.0, 0.0],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        s.op_radial_array(
            "blades",
            "blade",
            4,
            [0.0; 3],
            360.0,
            0.0,
            crate::Axis::Z,
            true,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        // Source + 3 copies at 90/180/270; no `blades_0` overlapping the source.
        assert!(s.model.part("blades_0").is_none());
        assert!(s.model.part("blades_3").is_some());
        let x = s
            .model
            .part("blades_1")
            .map(|p| p.translation[0])
            .unwrap_or(9.0);
        assert!(
            (x + 1.0).abs() < 1e-5,
            "90 degrees about z moves +y to -x, got {x}"
        );
        assert!(
            s.lints().iter().all(|l| !l.contains("overlap")),
            "{:?}",
            s.lints()
        );
    }

    #[test]
    fn expect_bounds_names_the_part_that_sets_the_extreme() {
        let mut s = op_session();
        let err = s
            .expect_bounds([None; 3], [None, Some(0.0), None], 0.01)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(err.contains("set by `"), "{err}");
        let _ = &mut s;
    }

    #[test]
    fn boolean_outputs_are_closed_and_lint_clean() {
        let mut s = Session::default();
        s.op_primitive(
            "a",
            crate::PrimitiveKind::Cylinder,
            [1.0, 2.0, 0.0],
            [0.0; 3],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        s.op_primitive(
            "b",
            crate::PrimitiveKind::Box,
            [0.5, 0.5, 3.0],
            [0.4, 0.0, 0.0],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        s.op_boolean("c", crate::BoolOp::Difference, "a", "b")
            .unwrap_or_else(|e| panic!("{e}"));
        let part = s.model.part("c").unwrap_or_else(|| panic!("c"));
        // Manifold outputs are closed, so the open-edge lint stays silent.
        assert_eq!(crate::verify::open_edge_count(part), 0);
        assert!(!s.lints().iter().any(|l| l.contains("open edges")));
    }

    #[test]
    fn params_rederive_dependent_ops() {
        use serde_json::json;
        let mut s = Session::default();
        s.apply_values(
            Some([("h".to_string(), 2.0)].into()),
            vec![
                json!({"type": "primitive", "name": "tower", "kind": "cylinder",
                       "size": [0.5, "$h"], "translation": [0, "=$h/2", 0]}),
                json!({"type": "primitive", "name": "roof", "kind": "cone",
                       "size": [0.6, 0.5], "translation": [0, "=$h + 0.25", 0]}),
            ],
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let top = |s: &Session| s.model.bounds().map(|(_, max)| max.y).unwrap_or(0.0);
        assert!((top(&s) - 2.5).abs() < 1e-4, "{}", top(&s));
        // One parameter change moves both ops.
        let out = s
            .apply_values(Some([("h".to_string(), 3.0)].into()), vec![])
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(out.contains("re-derived 2 op(s)"), "{out}");
        assert!((top(&s) - 3.5).abs() < 1e-4, "{}", top(&s));
        let tower = s.model.part("tower").unwrap_or_else(|| panic!("tower"));
        assert!((tower.translation[1] - 1.5).abs() < 1e-5);
        // Undo restores the old parameter with its geometry.
        s.undo().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.model.params.get("h"), Some(&2.0));
        // Unknown parameter: clear error, nothing applied.
        let Err(e) = s.apply_values(
            None,
            vec![json!({"type": "primitive", "name": "x", "kind": "box", "size": "$nope"})],
        ) else {
            panic!("unknown param should fail");
        };
        assert!(e.to_string().contains("unknown parameter `$nope`"), "{e}");
        assert!(s.model.part("x").is_none());
    }

    #[test]
    fn diff_and_cherry_pick_recover_a_branch() {
        use serde_json::json;
        let mut s = Session::default();
        s.apply_values(
            None,
            vec![json!({"type": "primitive", "name": "a", "kind": "box", "size": 1})],
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let base = s.model.history.current.unwrap_or_else(|| panic!("cursor"));
        s.apply_values(
            None,
            vec![
                json!({"type": "primitive", "name": "b", "kind": "sphere", "size": 0.5, "translation": [2, 0, 0]}),
                json!({"type": "primitive", "name": "a", "kind": "box", "size": 2}),
            ],
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let tip = s.model.history.current.unwrap_or_else(|| panic!("cursor"));
        let d = s.diff(base, None).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            d.contains("+ primitive `b`") && d.contains("~ primitive `a`"),
            "{d}"
        );
        // Jump back, branch, then pick only `b` from the old tip.
        s.jump(base).unwrap_or_else(|e| panic!("{e}"));
        assert!(s.model.part("b").is_none());
        s.cherry_pick(tip, Some(vec!["b".into()]))
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(s.model.part("b").is_some());
        let a = s.model.part("a").unwrap_or_else(|| panic!("a"));
        assert!(
            (a.bounds().1.x - 0.5).abs() < 1e-5,
            "`a` must keep its branch size"
        );
        assert!(s.cherry_pick(tip, Some(vec!["zzz".into()])).is_err());
    }

    #[test]
    fn param_template_survives_an_in_place_material() {
        use serde_json::json;
        let mut s = Session::default();
        s.apply_values(
            Some([("h".to_string(), 2.0)].into()),
            vec![
                json!({"type": "primitive", "name": "t", "kind": "cylinder", "size": [0.5, "$h"]}),
                json!({"type": "material", "name": "t", "color": [1, 0, 0]}),
            ],
        )
        .unwrap_or_else(|e| panic!("{e}"));
        s.apply_values(Some([("h".to_string(), 4.0)].into()), vec![])
            .unwrap_or_else(|e| panic!("{e}"));
        let t = s.model.part("t").unwrap_or_else(|| panic!("t"));
        assert!((t.bounds().1.y - 2.0).abs() < 1e-5, "height must follow h");
        assert_eq!(t.material.base_color, [1.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn an_apply_ops_call_is_one_undo_step() {
        use serde_json::json;
        let mut s = Session::default();
        s.new_model("t").unwrap_or_else(|e| panic!("{e}"));
        let before = s.model.history.nodes.len();
        s.apply_values(
            Some([("h".to_string(), 1.0)].into()),
            vec![
                json!({"type": "primitive", "name": "a", "kind": "box", "size": "$h"}),
                json!({"type": "primitive", "name": "b", "kind": "box", "size": 1, "translation": [3, 0, 0]}),
                json!({"type": "material", "name": "a", "color": "#f00"}),
            ],
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            s.model.history.nodes.len(),
            before + 1,
            "{}",
            s.history(None)
        );
        s.undo().unwrap_or_else(|e| panic!("{e}"));
        assert!(s.model.parts.is_empty(), "one undo reverts the whole call");
    }

    #[test]
    fn cherry_pick_brings_the_params_its_ops_were_built_with() {
        use serde_json::json;
        let mut s = Session::default();
        s.apply_values(
            Some([("h".to_string(), 1.0)].into()),
            vec![json!({"type": "primitive", "name": "a", "kind": "box", "size": [1, "$h", 1]})],
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let base = s
            .model
            .history
            .current
            .unwrap_or_else(|| panic!("no cursor"));
        s.apply_values(Some([("h".to_string(), 3.0)].into()), vec![])
            .unwrap_or_else(|e| panic!("{e}"));
        let tip = s
            .model
            .history
            .current
            .unwrap_or_else(|| panic!("no cursor"));
        s.jump(base).unwrap_or_else(|e| panic!("{e}"));
        s.apply_values(
            None,
            vec![json!({"type": "primitive", "name": "z", "kind": "box", "size": 1, "translation": [5, 0, 0]})],
        )
        .unwrap_or_else(|e| panic!("{e}"));
        // Picking the param by name works, and the op re-derives with it.
        s.cherry_pick(tip, Some(vec!["h".into()]))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.model.params.get("h"), Some(&3.0));
        let a = s.model.part("a").unwrap_or_else(|| panic!("no a"));
        let (lo, hi) = a.bounds();
        assert!((hi.y - lo.y - 3.0).abs() < 1e-5, "{lo} {hi}");
    }

    #[test]
    fn group_merges_disjoint_parts_and_arrays_as_one() {
        use serde_json::json;
        let mut s = Session::default();
        s.apply_values(
            None,
            vec![
                json!({"type": "primitive", "name": "t", "kind": "box", "size": 0.5}),
                json!({"type": "linear_array", "name": "t", "input": "t", "count": 3, "offset": [1, 0, 0]}),
                json!({"type": "group", "name": "row", "inputs": ["t", "t_1", "t_2"]}),
                json!({"type": "linear_array", "name": "rows", "input": "row", "count": 2, "offset": [0, 0, 1]}),
                json!({"type": "hide_part", "part": "t"}),
                json!({"type": "hide_part", "part": "t_1"}),
                json!({"type": "hide_part", "part": "t_2"}),
            ],
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let row = s.model.part("row").unwrap_or_else(|| panic!("no row"));
        assert_eq!(
            row.verts.len(),
            3 * s.model.part("t").map_or(0, |p| p.verts.len())
        );
        assert!(s.model.part("rows_1").is_some());
        let (_, hi) = s
            .model
            .part("rows_1")
            .unwrap_or_else(|| panic!("x"))
            .bounds();
        assert!(
            (hi.x - 2.25).abs() < 1e-5 && (hi.z - 1.25).abs() < 1e-5,
            "{hi}"
        );
        assert!(
            s.lints().iter().all(|l| !l.contains("no overlap")),
            "{:?}",
            s.lints()
        );
        let spec: VerifySpec = serde_json::from_value(
            json!({"parts": ["row", "rows_*"], "includes": ["rows_1"], "grounded": true, "validate": true}),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let checks = s.verify_checks(&spec);
        assert!(checks.iter().all(|c| c.1), "{checks:?}");
    }

    #[test]
    fn a_part_hanging_in_the_air_is_flagged() {
        use serde_json::json;
        let mut s = Session::default();
        s.apply_values(
            None,
            vec![
                json!({"type": "primitive", "name": "pole", "kind": "cylinder", "size": [0.03, 2], "translation": [0, 1, 0]}),
                json!({"type": "primitive", "name": "cap", "kind": "box", "size": 0.1, "translation": [0, 2.05, 0]}),
                json!({"type": "transform", "name": "stray", "input": "pole", "translation": [0.5, 3, 0]}),
            ],
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let lints = s.lints();
        assert!(
            lints.iter().any(|l| l.contains("`stray` floats")),
            "{lints:?}"
        );
        assert!(
            !lints.iter().any(|l| l.contains("`cap` floats")),
            "{lints:?}"
        );
    }

    #[test]
    fn reissuing_an_array_named_after_its_source_replaces_it() {
        use serde_json::json;
        let mut s = Session::default();
        let batch = vec![
            json!({"type": "primitive", "name": "tile", "kind": "box", "size": 0.2}),
            json!({"type": "linear_array", "name": "tile", "input": "tile", "count": 3, "offset": [0.3, 0, 0]}),
        ];
        s.apply_values(None, batch.clone())
            .unwrap_or_else(|e| panic!("{e}"));
        s.apply_values(None, batch)
            .unwrap_or_else(|e| panic!("re-issue failed: {e}"));
        assert_eq!(s.model.ops.len(), 2, "{}", s.describe_ops());
    }

    #[test]
    fn brief_apply_result_lists_only_what_changed() {
        let mut s = Session::default();
        s.new_model("t").unwrap_or_else(|e| panic!("{e}"));
        let ops = |v: serde_json::Value| -> Vec<serde_json::Value> {
            serde_json::from_value(v).unwrap_or_else(|e| panic!("{e}"))
        };
        let t = s
            .apply_values_as(
                None,
                ops(serde_json::json!([
                    {"type": "primitive", "name": "a", "kind": "box", "size": 1, "translation": [0, 0.5, 0]},
                    {"type": "primitive", "name": "b", "kind": "box", "size": 1, "translation": [3, 0.5, 0]}
                ])),
                false,
            )
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(t.contains("parts (2 changed): +a, +b"), "{t}");
        assert!(!t.contains("ops ("), "{t}");
        let t = s
            .apply_values_as(
                None,
                ops(serde_json::json!([
                    {"type": "primitive", "name": "b", "kind": "box", "size": 2, "translation": [3, 1, 0]}
                ])),
                false,
            )
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(t.contains("parts (1 changed): ~b"), "{t}");
        let spec = VerifySpec {
            stable: Some(true),
            ..Default::default()
        };
        assert!(s.verify(&spec).0);
    }

    #[test]
    fn delete_takes_own_edits_and_hide_takes_globs() {
        let mut s = Session::default();
        s.new_model("t").unwrap_or_else(|e| panic!("{e}"));
        let ops: Vec<serde_json::Value> = serde_json::from_value(serde_json::json!([
            {"type": "primitive", "name": "cut", "kind": "box", "size": 1},
            {"type": "linear_array", "name": "cut", "input": "cut", "count": 3, "offset": [2, 0, 0]},
            {"type": "hide_part", "part": "cut*"},
            {"type": "material", "name": "cut", "color": "#f00"}
        ]))
        .unwrap_or_else(|e| panic!("{e}"));
        let t = s
            .apply_values_as(None, ops, false)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(t.contains("3 part(s) hidden by `cut*`"), "{t}");
        assert!(s.model.parts.iter().all(|p| !p.visible));
        let ops: Vec<serde_json::Value> =
            serde_json::from_value(serde_json::json!([{"type": "delete", "name": "cut_2"}]))
                .unwrap_or_else(|e| panic!("{e}"));
        // cut_2 is an array output, not an op; deleting the source op
        // with its own hide/material edits must work.
        let _ = s.apply_values(None, ops);
        let ops: Vec<serde_json::Value> = serde_json::from_value(serde_json::json!([
            {"type": "delete", "name": "cut"}
        ]))
        .unwrap_or_else(|e| panic!("{e}"));
        let r = s.apply_values(None, ops);
        assert!(r.is_err(), "the array still reads `cut`");
    }

    #[test]
    fn move_turns_a_set_of_parts_as_one_body() {
        let mut s = Session::default();
        s.new_model("t").unwrap_or_else(|e| panic!("{e}"));
        let v = |j: serde_json::Value| -> Vec<serde_json::Value> {
            serde_json::from_value(j).unwrap_or_else(|e| panic!("{e}"))
        };
        s.apply_values(
            None,
            v(serde_json::json!([
                {"type": "primitive", "name": "a", "kind": "box", "size": 1, "translation": [0, 0.5, 0]},
                {"type": "linear_array", "name": "a", "input": "a", "count": 2, "offset": [2, 0, 0]}
            ])),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        // Shift everything up 1: the copy must move once, not twice.
        s.apply_values(
            None,
            v(serde_json::json!([{"type": "move", "parts": ["*"], "translation": [0, 1, 0]}])),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        for n in ["a", "a_1"] {
            let (lo, _) = s.model.part(n).map(|p| p.bounds()).unwrap_or_default();
            assert!((lo.y - 1.0).abs() < 1e-4, "{n} at {lo:?}");
        }
        // Quarter turn about z through [1, 0, 0]: the pair stands up.
        s.apply_values(
            None,
            v(serde_json::json!([{"type": "move", "parts": ["a*"], "axis": [0, 0, 1], "degrees": 90, "pivot": [-0.5, 1, 0]}])),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let (lo, hi) = s.model.bounds().unwrap_or_default();
        assert!((hi.x - lo.x - 1.0).abs() < 1e-3, "{lo:?}..{hi:?}");
        assert!((hi.y - lo.y - 3.0).abs() < 1e-3, "{lo:?}..{hi:?}");
        assert!((lo.y - 1.0).abs() < 1e-3, "{lo:?}");
    }

    #[test]
    fn move_tilts_a_mirror_once() {
        let mut s = Session::default();
        s.new_model("t").unwrap_or_else(|e| panic!("{e}"));
        let v = |j: serde_json::Value| -> Vec<serde_json::Value> {
            serde_json::from_value(j).unwrap_or_else(|e| panic!("{e}"))
        };
        s.apply_values(
            None,
            v(serde_json::json!([
                {"type": "primitive", "name": "a", "kind": "box", "size": [0.1, 1, 0.1], "translation": [0, 0.5, 0.3]},
                {"type": "mirror", "name": "a_m", "input": "a", "axis": "z"},
                {"type": "move", "parts": ["*"], "axis": [1, 0, 0], "degrees": 10, "pivot": [0, 0, 0]}
            ])),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let (alo, ahi) = s.model.part("a").map(|p| p.bounds()).unwrap_or_default();
        let (mlo, mhi) = s.model.part("a_m").map(|p| p.bounds()).unwrap_or_default();
        // Rigid: both lean by the same 10° (a double tilt would widen the
        // mirror's z extent), and the mirror's foot lifts as it swings.
        assert!(
            ((ahi.z - alo.z) - (mhi.z - mlo.z)).abs() < 1e-4,
            "a {alo:?}..{ahi:?} m {mlo:?}..{mhi:?}"
        );
        assert!(
            mlo.y > 0.04 && alo.y < -0.06,
            "a {alo:?}..{ahi:?} m {mlo:?}..{mhi:?}"
        );
    }
}
