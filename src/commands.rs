//! The tool operations as plain functions on a [`Session`] (model + undo history). Pure,
//! unit-testable, wiring-free API used by both the MCP tools and tests.

use crate::{Annotation, Axis, BoolOp, Model, ModelerError, Op, Part, PrimitiveKind, graph};

pub type CmdResult = Result<String, ModelerError>;

/// One item in an `apply_batch` call: either a graph op or a baked edit
/// (hide/show/rename) that runs in batch position.
#[derive(Debug)]
pub enum BatchItem {
    Op(Op),
    Hide { part: String },
    Show { part: String },
    Rename { part: String, to: String },
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
}

impl Default for Session {
    fn default() -> Self {
        Self {
            model: Model::named("model"),
            pending_pin: None,
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
        self.model.version += 1;
        // The snapshot carries no tree of its own: the live history on the
        // session model is the single source of truth for the tree shape.
        let mut snapshot = self.model.clone();
        snapshot.history = crate::History::default();
        self.model.history.record(snapshot, label);
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
        let prev = self.model.ops.iter().rposition(|o| {
            graph::output(o) == out
                && graph::kind(o) == kind
                && (replace_in_place || !reads.contains(&graph::output(o)))
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
                i != idx
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
        self.model.ops.retain(|o| graph::output(o) != name);
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

    /// Applies a mixed batch: graph ops and baked edits (hide/show/rename),
    /// each running in its listed position with per-item error reporting.
    /// Graph ops keep the upsert/rollback semantics of `apply_op`.
    pub fn apply_batch(&mut self, items: Vec<BatchItem>) -> CmdResult {
        let mut lines = Vec::with_capacity(items.len());
        for (i, item) in items.into_iter().enumerate() {
            let msg = match item {
                BatchItem::Op(op) => {
                    let label = Self::op_label(&op);
                    self.apply_op(op, &label)
                        .map_err(|e| format!("batch item #{i} (`{label}`) failed: {e}"))?;
                    format!("{label} ok")
                }
                BatchItem::Hide { part } => self
                    .set_visible(&part, false)
                    .map_err(|e| format!("batch item #{i} failed: {e}"))?,
                BatchItem::Show { part } => self
                    .set_visible(&part, true)
                    .map_err(|e| format!("batch item #{i} failed: {e}"))?,
                BatchItem::Rename { part, to } => self
                    .rename_part(&part, &to)
                    .map_err(|e| format!("batch item #{i} failed: {e}"))?,
            };
            lines.push(format!("#{i} {msg}"));
        }
        lines.push(self.summary());
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

    /// The whole history tree, one line per snapshot.
    pub fn history(&self) -> String {
        self.model.history.describe()
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
        assert!(s.history().contains('#'));
    }

    #[test]
    fn editing_after_undo_branches_instead_of_discarding() {
        let mut s = session_with_part();
        s.rename_part("leg", "b").unwrap_or_else(|e| panic!("{e}"));
        undo(&mut s);
        // A new edit while back at the older state: the rename stays reachable.
        s.rename_part("leg", "c").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.model.parts[0].name, "c");
        assert!(s.history().contains("`rename `leg` -> `b`"));
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
    fn lint_material_after_copy() {
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
        s.op_material("arm", "arm", Some([1.0, 0.0, 0.0, 1.0]), None, None)
            .unwrap_or_else(|e| panic!("{e}"));
        let lints = s.lints();
        assert!(
            lints
                .iter()
                .any(|l| l.contains("kept the default material")),
            "lints: {lints:?}"
        );
        // And the part list really does show the copy uncolored.
        assert!(
            s.summary()
                .contains("part `arm_r` still has the default material")
        );
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
                    scale: None
                },
                Op::Transform {
                    name: "b".into(),
                    input: "a".into(),
                    translation: None,
                    rotation: None,
                    scale: None
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
}
