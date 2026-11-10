//! MCP-driven mesh modeling: build models from primitives, boolean operations and raw
//! triangle meshes, with correct UV mappings, export as glTF. No game code.

pub mod catalog;
pub mod commands;
pub mod csg;
pub mod gltf;
pub mod graph;
pub mod primitives;
pub mod uv;

use bevy::math::{Quat, Vec3};
use serde::{Deserialize, Serialize};

/// A named material. Placeholder art is fine: base color + a couple of knobs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Material {
    pub base_color: [f32; 4],
    pub roughness: f32,
    pub metalness: f32,
}

impl Default for Material {
    fn default() -> Self {
        Self {
            base_color: [0.7, 0.7, 0.7, 1.0],
            roughness: 0.6,
            metalness: 0.0,
        }
    }
}

/// A triangle mesh part of the model, in model-local space.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Part {
    pub name: String,
    pub verts: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
    /// Local transform relative to the model root (and any future parent node).
    pub translation: [f32; 3],
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
    pub material: Material,
}

impl Part {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            translation: [0.0; 3],
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0; 3],
            ..Default::default()
        }
    }

    pub fn tri_count(&self) -> u32 {
        (self.indices.len() / 3) as u32
    }

    /// World-space bounds: the mesh bounds through this part's transform.
    pub fn bounds(&self) -> (Vec3, Vec3) {
        let rot = quat_of(self.rotation);
        let scale = Vec3::from(self.scale);
        let mut min = Vec3::splat(f32::MAX);
        let mut max = Vec3::splat(f32::MIN);
        for v in &self.verts {
            let v = rot * (Vec3::from(*v) * scale) + Vec3::from(self.translation);
            min = min.min(v);
            max = max.max(v);
        }
        (min, max)
    }
}

/// The whole model document: a flat list of parts, each its own node for now.
/// Joints/skin hang off this later; nothing here knows about the game.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Model {
    pub name: String,
    pub parts: Vec<Part>,
    /// Bumped on every mutation so observers (the preview app) know to rebuild.
    pub version: u64,
    /// The procedural graph the parts derive from, when the model is built
    /// with ops. Empty means the parts are hand-authored/baked. When ops are
    /// present, `parts` is just the cached evaluation of this list.
    #[serde(default)]
    pub ops: Vec<Op>,
    /// Human/AI comments pinned to spots on the model. Carried in the prefab JSON.
    #[serde(default)]
    pub annotations: Vec<Annotation>,
    /// Next id to hand out for an annotation; ids never repeat within a model.
    #[serde(default)]
    pub next_annotation_id: u32,
    /// The edit tree this model grew through. Carried in prefabs so a save is
    /// the full story, not just the final geometry. Snapshot models nest no
    /// further history (stripped when serialized), so this stays a tree, not a
    /// recursion.
    #[serde(default, skip_serializing_if = "History::is_empty")]
    pub history: History,
}

impl Model {
    /// A fresh model with a sensible display name.
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }
    pub fn part(&self, name: &str) -> Option<&Part> {
        self.parts.iter().find(|p| p.name == name)
    }

    /// True when the parts derive from the op graph; `parts` is then a cache.
    pub fn procedural(&self) -> bool {
        !self.ops.is_empty()
    }

    /// Re-derives `parts` from the op graph. A no-op for baked models.
    pub fn refresh(&mut self) -> Result<(), String> {
        if self.ops.is_empty() {
            return Ok(());
        }
        self.parts = crate::graph::evaluate(&self.ops)?;
        Ok(())
    }

    /// True when every cached part derives from the ops, so dropping `parts`
    /// from a save loses nothing. Mixed models (baked parts plus ops) keep
    /// their parts on disk.
    pub fn parts_derive_from_ops(&self) -> bool {
        if !self.procedural() {
            return false;
        }
        match crate::graph::evaluate(&self.ops) {
            Ok(derived) => {
                derived.len() == self.parts.len()
                    && derived
                        .iter()
                        .zip(&self.parts)
                        .all(|(d, p)| d.name == p.name)
            }
            Err(_) => false,
        }
    }

    pub fn part_mut(&mut self, name: &str) -> Option<&mut Part> {
        self.parts.iter_mut().find(|p| p.name == name)
    }

    pub fn annotation_at(&self, id: u32) -> Option<&Annotation> {
        self.annotations.iter().find(|a| a.id == id)
    }

    /// Mean of a part's vertices, offset by the part's translation. A rough
    /// "middle of the part" for pinning annotations without a clicked spot.
    pub fn part_centroid(&self, name: &str) -> Option<[f32; 3]> {
        let p = self.part(name)?;
        if p.verts.is_empty() {
            return Some(p.translation);
        }
        let n = p.verts.len() as f32;
        let mut sum = Vec3::ZERO;
        for v in &p.verts {
            sum += Vec3::from(*v);
        }
        let c = Vec3::from(p.translation) + sum / n;
        Some(c.to_array())
    }

    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        let mut acc: Option<(Vec3, Vec3)> = None;
        for p in &self.parts {
            if p.verts.is_empty() {
                continue;
            }
            let (min, max) = p.bounds();
            acc = Some(match acc {
                None => (min, max),
                Some((amin, amax)) => (amin.min(min), amax.max(max)),
            });
        }
        acc
    }
}

/// One snapshot of the whole model, plus its place in the edit tree. Editing at
/// an older node branches: the new snapshot becomes a sibling of what used to
/// follow it, so abandoned tries stay reachable instead of being lost.
///
/// Serialization is non-destructive and small: a snapshot of a procedural
/// model stores its ops (parts are re-derived on load) and never nests a
/// second history inside itself.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub model: Model,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub label: String,
}

impl Serialize for Snapshot {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut m = self.model.clone();
        if m.parts_derive_from_ops() {
            // Parts are the cached evaluation of ops; drop them from the file.
            m.parts.clear();
        }
        m.history = History::default();
        let mut st = s.serialize_struct("Snapshot", 4)?;
        st.serialize_field("model", &m)?;
        st.serialize_field("parent", &self.parent)?;
        st.serialize_field("children", &self.children)?;
        st.serialize_field("label", &self.label)?;
        st.end()
    }
}

impl<'de> Deserialize<'de> for Snapshot {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            model: Model,
            #[serde(default)]
            parent: Option<usize>,
            #[serde(default)]
            children: Vec<usize>,
            #[serde(default)]
            label: String,
        }
        let raw = Raw::deserialize(d)?;
        Ok(Snapshot {
            model: raw.model,
            parent: raw.parent,
            children: raw.children,
            label: raw.label,
        })
    }
}

/// The history of a session as a tree of snapshots with a cursor. Every
/// mutation records a child of the current node; undo walks to the parent,
/// redo to the most recent child, and jump lands anywhere. Navigation is O(1)
/// and exact: nodes hold full models, never inverse operations.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct History {
    pub nodes: Vec<Snapshot>,
    #[serde(default)]
    pub current: Option<usize>,
}

impl History {
    /// True when nothing has been recorded yet (skipped in serialization).
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Records `model` as a child of the current node, makes it current and
    /// returns its id.
    pub fn record(&mut self, model: Model, label: impl Into<String>) -> usize {
        let id = self.nodes.len();
        self.nodes.push(Snapshot {
            model,
            parent: self.current,
            children: Vec::new(),
            label: label.into(),
        });
        if let Some(p) = self.current {
            self.nodes[p].children.push(id);
        }
        self.current = Some(id);
        id
    }

    /// Moves the cursor to the parent and returns its id and model (owned, so
    /// the caller can hand it back into a model that owns this history).
    pub fn undo(&mut self) -> Option<(usize, Model)> {
        let parent = self.nodes[self.current?].parent?;
        self.current = Some(parent);
        Some((parent, self.nodes[parent].model.clone()))
    }

    /// Moves the cursor to the most recent child and returns its id and model.
    pub fn redo(&mut self) -> Option<(usize, Model)> {
        let id = *self.nodes[self.current?].children.last()?;
        self.current = Some(id);
        Some((id, self.nodes[id].model.clone()))
    }

    /// Moves the cursor to any node in the tree and returns its id and model.
    pub fn jump(&mut self, id: usize) -> Result<(usize, Model), String> {
        let Some(node) = self.nodes.get(id) else {
            return Err(format!("no history node #{id} (0..{})", self.nodes.len()));
        };
        self.current = Some(id);
        Ok((id, node.model.clone()))
    }

    /// One line per node: id, parent, label, model version and the cursor.
    pub fn describe(&self) -> String {
        if self.nodes.is_empty() {
            return "history is empty".into();
        }
        let mut lines = Vec::new();
        for (id, node) in self.nodes.iter().enumerate() {
            let cursor = if self.current == Some(id) { " <-" } else { "" };
            let parent = node
                .parent
                .map(|p| format!("parent #{p}"))
                .unwrap_or_else(|| "root".into());
            let label = if node.label.is_empty() {
                "(no label)".to_string()
            } else {
                format!("`{}`", node.label)
            };
            lines.push(format!(
                "#{id} {parent}, {label}, v{}{cursor}",
                node.model.version
            ));
        }
        lines.join("\n")
    }
}

/// One step of the procedural graph behind the parts. Ops are upserted by
/// output name: re-issuing an op with the same `name` replaces it, which is
/// how a parameter gets tweaked without rebuilding the whole graph.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    /// A primitive from [`crate::primitives`] at a fixed spot.
    Primitive {
        name: String,
        kind: String,
        size: [f32; 3],
        translation: [f32; 3],
    },
    /// Places/rotates/scales an earlier part into a new output.
    Transform {
        name: String,
        input: String,
        #[serde(default)]
        translation: Option<[f32; 3]>,
        #[serde(default)]
        rotation: Option<[f32; 4]>,
        #[serde(default)]
        scale: Option<[f32; 3]>,
    },
    /// CSG combine two earlier parts (`union`/`difference`/`intersection`).
    Boolean {
        name: String,
        op: String,
        a: String,
        b: String,
    },
    /// Copies an earlier part around a circle: `count` copies spread over
    /// `degrees` starting at `start_degrees`, rotating about the +Y axis
    /// through `center`. With start = step and degrees = 360 - step, the
    /// source part fills the 0° slot evenly.
    RadialArray {
        name: String,
        input: String,
        count: u32,
        #[serde(default)]
        center: [f32; 3],
        #[serde(default = "default_degrees")]
        degrees: f32,
        #[serde(default)]
        start_degrees: f32,
    },
    /// Re-materials an earlier part into a new output.
    Material {
        name: String,
        input: String,
        #[serde(default)]
        base_color: Option<[f32; 4]>,
        #[serde(default)]
        roughness: Option<f32>,
        #[serde(default)]
        metalness: Option<f32>,
    },
    /// Copies an earlier part in a straight line. The source is copy 0;
    /// copies 1..count are emitted as `{name}_1..{name}_{count-1}`, copy i
    /// sitting at input + i * offset.
    LinearArray {
        name: String,
        input: String,
        count: u32,
        #[serde(default)]
        offset: [f32; 3],
    },
    /// Mirrors an earlier part across the plane `axis = offset[axis]` (axis is
    /// "x", "y" or "z"). The mesh is baked: verts reflected, winding flipped,
    /// normals rebuilt — so it renders correctly, unlike a negative scale.
    Mirror {
        name: String,
        input: String,
        axis: String,
        #[serde(default)]
        offset: [f32; 3],
    },
}

fn default_degrees() -> f32 {
    360.0
}

/// A quat from the flat `[x, y, z, w]` storage used in [`Part`].
pub fn quat_of(r: [f32; 4]) -> Quat {
    Quat::from_xyzw(r[0], r[1], r[2], r[3])
}

/// A human comment pinned to a spot on the model: a part, optionally a specific
/// triangle face or vertex index, with the world-space point where it was pinned.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Annotation {
    pub id: u32,
    pub part: String,
    pub face: Option<u32>,
    pub vertex: Option<u32>,
    pub position: [f32; 3],
    pub text: String,
}
