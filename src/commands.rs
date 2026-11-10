//! The tool operations as plain functions on a [`Session`] (model + undo history). Pure,
//! unit-testable, wiring-free API used by both the MCP tools and tests.

use crate::{Annotation, Model, Op, Part, graph};

pub type CmdResult = Result<String, String>;

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
    fn bump(&mut self) {
        self.model.version += 1;
    }

    /// Records the just-mutated model in the history tree under `label`. The
    /// tree lives on the model, so it travels with saves and prefab loads.
    fn commit(&mut self, label: &str) {
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
        let prev = self.model.ops.iter().rposition(|o| {
            graph::output(o) == out && graph::kind(o) == kind && !reads.contains(&graph::output(o))
        });
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
        self.bump();
        self.commit(label);
        Ok(format!("{label} ok"))
    }

    /// A primitive as a graph step (upsert by output name).
    pub fn op_primitive(
        &mut self,
        name: &str,
        kind: &str,
        size: [f32; 3],
        translation: [f32; 3],
    ) -> CmdResult {
        self.apply_op(
            Op::Primitive {
                name: name.into(),
                kind: kind.into(),
                size,
                translation,
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

    pub fn op_boolean(&mut self, name: &str, op: &str, a: &str, b: &str) -> CmdResult {
        self.apply_op(
            Op::Boolean {
                name: name.into(),
                op: op.into(),
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
    ) -> CmdResult {
        self.apply_op(
            Op::RadialArray {
                name: name.into(),
                input: input.into(),
                count,
                center,
                degrees,
                start_degrees,
            },
            &format!("op radial `{name}` = {count} x `{input}` over {degrees}°"),
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
        axis: &str,
        offset: [f32; 3],
    ) -> CmdResult {
        self.apply_op(
            Op::Mirror {
                name: name.into(),
                input: input.into(),
                axis: axis.into(),
                offset,
            },
            &format!("op mirror `{name}` = `{input}` across {axis}"),
        )
    }

    /// Removes the op whose output is `name` and re-derives.
    pub fn op_delete(&mut self, name: &str) -> CmdResult {
        let before = self.model.ops.len();
        self.model.ops.retain(|o| graph::output(o) != name);
        if self.model.ops.len() == before {
            return Err(format!("no op with output `{name}`"));
        }
        self.model.refresh()?;
        self.bump();
        self.commit(&format!("op delete `{name}`"));
        Ok(format!("deleted op `{name}`"))
    }

    /// The op graph, one line per step.
    pub fn describe_ops(&self) -> String {
        graph::describe(&self.model.ops)
    }

    pub fn add_primitive(
        &mut self,
        kind: &str,
        size: [f32; 3],
        translation: [f32; 3],
        name: &str,
    ) -> CmdResult {
        let mut part = crate::primitives::primitive(kind, size, name).map_err(|e| e.to_string())?;
        part.translation = translation;
        self.model.parts.push(part);
        self.bump();
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
        self.model.parts.push(part);
        self.bump();
        self.commit(&format!("add tri mesh `{name}`"));
        Ok(format!("added tri mesh `{name}` ({tris} tris)"))
    }

    pub fn boolean(&mut self, op: &str, a: &str, b: &str, name: Option<&str>) -> CmdResult {
        let out = crate::csg::boolean(&self.model, op, a, b, name)?;
        self.model.parts.retain(|p| p.name != a && p.name != b);
        self.model.parts.push(out);
        self.bump();
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
        let p = self
            .model
            .part_mut(part)
            .ok_or_else(|| format!("no part `{part}`"))?;
        if let Some(t) = translation {
            p.translation = t;
        }
        if let Some(r) = rotation {
            p.rotation = r;
        }
        if let Some(s) = scale {
            p.scale = s;
        }
        self.bump();
        self.commit(&format!("transform `{part}`"));
        Ok(format!("transformed `{part}`"))
    }

    pub fn delete_part(&mut self, part: &str) -> CmdResult {
        let before = self.model.parts.len();
        self.model.parts.retain(|p| p.name != part);
        if self.model.parts.len() == before {
            Err(format!("no part `{part}`"))
        } else {
            self.bump();
            self.commit(&format!("delete `{part}`"));
            Ok(format!("deleted `{part}`"))
        }
    }

    pub fn rename_part(&mut self, part: &str, to: &str) -> CmdResult {
        let p = self
            .model
            .part_mut(part)
            .ok_or_else(|| format!("no part `{part}`"))?;
        p.name = to.to_string();
        self.bump();
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
        let p = self
            .model
            .part_mut(part)
            .ok_or_else(|| format!("no part `{part}`"))?;
        if let Some(c) = base_color {
            p.material.base_color = c;
        }
        if let Some(r) = roughness {
            p.material.roughness = r;
        }
        if let Some(m) = metalness {
            p.material.metalness = m;
        }
        self.bump();
        self.commit(&format!("material on `{part}`"));
        Ok(format!("material on `{part}` updated"))
    }

    pub fn set_uvs(&mut self, part: &str, projection: &str) -> CmdResult {
        let p = self
            .model
            .part_mut(part)
            .ok_or_else(|| format!("no part `{part}`"))?;
        crate::uv::project(p, projection).map_err(|e| e.to_string())?;
        self.bump();
        self.commit(&format!("uvs on `{part}`"));
        Ok(format!("uvs on `{part}` set to {projection}"))
    }

    pub fn undo(&mut self) -> CmdResult {
        let (id, mut model) = self.model.history.undo().ok_or("nothing to undo")?;
        model.refresh()?;
        model.history = std::mem::take(&mut self.model.history);
        self.model = model;
        Ok(format!("undone to #{id}, model at v{}", self.model.version))
    }

    pub fn redo(&mut self) -> CmdResult {
        let (id, mut model) = self.model.history.redo().ok_or("nothing to redo")?;
        model.refresh()?;
        model.history = std::mem::take(&mut self.model.history);
        self.model = model;
        Ok(format!("redone to #{id}, model at v{}", self.model.version))
    }

    /// Jump the cursor anywhere in the history tree. Editing after a jump
    /// branches: the old future stays reachable as a sibling subtree.
    pub fn jump(&mut self, id: usize) -> CmdResult {
        let (_, mut model) = self.model.history.jump(id)?;
        model.refresh()?;
        model.history = std::mem::take(&mut self.model.history);
        self.model = model;
        Ok(format!(
            "jumped to #{id}, model at v{} (editing here branches)",
            self.model.version
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
                .ok_or_else(|| format!("no part `{part}`"))?;
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
            Err(format!("no annotation #{id}"))
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
                "- `{}`: {} verts {} tris, mat {:?}, at {:?}\n",
                p.name,
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
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_with_part() -> Session {
        let mut s = Session::default();
        s.add_primitive("box", [1.0, 1.0, 1.0], [0.0, 0.0, 0.0], "leg")
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
        s.op_primitive("leg", "box", [1.0, 1.0, 1.0], [0.0, 0.5, 0.0])
            .unwrap_or_else(|e| panic!("{e}"));
        s.op_primitive("ball", "sphere", [0.5, 0.0, 0.0], [0.0, 2.0, 0.0])
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
        s.op_primitive("stem", "cylinder", [0.05, 1.0, 0.0], [0.0, 0.5, 0.0])
            .unwrap_or_else(|e| panic!("{e}"));
        s.op_primitive("petal", "sphere", [0.2, 0.0, 0.0], [0.0, 1.0, 0.3])
            .unwrap_or_else(|e| panic!("{e}"));
        s.op_radial_array("ring", "petal", 6, [0.0, 1.0, 0.0], 360.0, 0.0)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(s.model.procedural());
        // stem + petal + 6 ring copies
        assert_eq!(s.model.parts.len(), 8);
        assert!(s.summary().contains("ops (3)"));

        // Re-issuing the radial op with another count is the tweak path.
        let v = s.model.version;
        s.op_radial_array("ring", "petal", 8, [0.0, 1.0, 0.0], 360.0, 0.0)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.model.parts.len(), 10);
        assert_eq!(s.model.ops.len(), 3);
        assert!(s.model.version > v);
    }

    #[test]
    fn bad_op_is_rejected_and_leaves_graph_intact() {
        let mut s = Session::default();
        s.op_primitive("a", "box", [1.0, 1.0, 1.0], [0.0, 0.0, 0.0])
            .unwrap_or_else(|e| panic!("{e}"));
        let v = s.model.version;
        assert!(s.op_boolean("cut", "difference", "a", "ghost").is_err());
        assert_eq!(s.model.ops.len(), 1);
        assert_eq!(s.model.version, v);
        assert_eq!(s.model.parts.len(), 1);
    }

    #[test]
    fn op_delete_rederives() {
        let mut s = Session::default();
        s.op_primitive("a", "box", [1.0, 1.0, 1.0], [0.0, 0.0, 0.0])
            .unwrap_or_else(|e| panic!("{e}"));
        s.op_primitive("b", "box", [1.0, 1.0, 1.0], [1.0, 0.0, 0.0])
            .unwrap_or_else(|e| panic!("{e}"));
        s.op_delete("a").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.model.parts.len(), 1);
        assert!(s.op_delete("a").is_err());
    }
}
