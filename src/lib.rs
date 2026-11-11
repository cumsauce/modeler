//! MCP-driven mesh modeling: build models from primitives, boolean operations and raw
//! triangle meshes, with correct UV mappings, export as glTF. No game code.

pub mod catalog;
pub mod commands;
pub mod csg;
pub mod error;
pub mod gltf;
pub mod graph;
pub mod primitives;
pub mod uv;
pub mod verify;

use bevy::math::{Quat, Vec3};
pub use error::ModelerError;
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
    /// Hidden parts stay in the model (and derive from ops as before) but are
    /// skipped by the preview render and the glTF export — useful for boolean
    /// intermediates that later ops still reference. Absent in JSON when true,
    /// so older prefabs load as visible.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub visible: bool,
}

fn default_true() -> bool {
    true
}

fn is_true(v: &bool) -> bool {
    *v
}

impl Part {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            translation: [0.0; 3],
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0; 3],
            visible: true,
            ..Default::default()
        }
    }

    pub fn tri_count(&self) -> u32 {
        (self.indices.len() / 3) as u32
    }

    /// World-space bounds: the mesh bounds through this part's transform.
    /// Vertex `i` in world space (scale, then rotation, then translation).
    pub fn world_vert(&self, i: usize) -> Vec3 {
        quat_of(self.rotation) * (Vec3::from(self.verts[i]) * Vec3::from(self.scale))
            + Vec3::from(self.translation)
    }

    pub fn bounds(&self) -> (Vec3, Vec3) {
        let mut min = Vec3::splat(f32::MAX);
        let mut max = Vec3::splat(f32::MIN);
        for i in 0..self.verts.len() {
            let v = self.world_vert(i);
            min = min.min(v);
            max = max.max(v);
        }
        (min, max)
    }
}

/// Current save-format version, written into every prefab and the autosave.
/// Bump it when the serialized shape changes incompatibly.
pub const FORMAT_VERSION: u32 = 1;

/// The whole model document: a flat list of parts, each its own node for now.
/// Joints/skin hang off this later; nothing here knows about the game.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Model {
    /// On-disk schema version (see [`FORMAT_VERSION`]). Files written before
    /// it existed read as 0 and load unchanged.
    #[serde(default)]
    pub format_version: u32,
    pub name: String,
    /// Shared so history snapshots reuse unchanged parts instead of cloning
    /// their meshes; mutate through `Arc::make_mut` (copy-on-write).
    pub parts: Vec<std::sync::Arc<Part>>,
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
    /// Set by a successful `refresh` on an op-driven model: the cached parts
    /// are exactly the graph's derivation, so saves can omit them. Baked-part
    /// edits clear it (the cache then carries information the ops don't).
    /// A stored flag instead of a re-evaluation: serialization walks every
    /// history snapshot, and re-deriving each one re-runs booleans (the old
    /// csgrs backend could take minutes) — that was the daemon hang.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub derived_from_ops: bool,
}

impl Model {
    /// A fresh model with a sensible display name.
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            format_version: FORMAT_VERSION,
            name: name.into(),
            ..Default::default()
        }
    }

    /// Rejects models saved by a newer build than this one understands.
    pub fn check_format(&self) -> Result<(), ModelerError> {
        if self.format_version > FORMAT_VERSION {
            return Err(ModelerError::Parse(format!(
                "model `{}` has format_version {} but this build only understands up to {FORMAT_VERSION}; upgrade modeler",
                self.name, self.format_version
            )));
        }
        Ok(())
    }
    pub fn part(&self, name: &str) -> Option<&Part> {
        self.parts.iter().map(|p| &**p).find(|p| p.name == name)
    }

    /// True when the parts derive from the op graph; `parts` is then a cache.
    pub fn procedural(&self) -> bool {
        !self.ops.is_empty()
    }

    /// Re-derives `parts` from the op graph. A no-op for baked models.
    pub fn refresh(&mut self) -> Result<(), ModelerError> {
        if self.ops.is_empty() {
            self.derived_from_ops = false;
            return Ok(());
        }
        self.parts = crate::graph::evaluate(&self.ops)?
            .into_iter()
            .map(std::sync::Arc::new)
            .collect();
        self.derived_from_ops = true;
        Ok(())
    }

    /// Like [`Model::refresh`] but bypasses the eval cache, for verification.
    pub fn refresh_uncached(&mut self) -> Result<(), ModelerError> {
        if self.ops.is_empty() {
            self.derived_from_ops = false;
            return Ok(());
        }
        self.parts = crate::graph::evaluate_uncached(&self.ops)?
            .into_iter()
            .map(std::sync::Arc::new)
            .collect();
        self.derived_from_ops = true;
        Ok(())
    }

    /// Re-derives the parts only when they are missing from an op-driven model
    /// (it was serialized without them). Models that carry their parts keep
    /// them, so baked edits (hide, rename, material...) survive undo/redo/load.
    pub fn restore_parts(&mut self) -> Result<(), ModelerError> {
        if self.procedural() && self.parts.is_empty() {
            self.refresh()?;
        }
        Ok(())
    }

    /// True when every cached part derives from the ops, so dropping `parts`
    /// from a save loses nothing. Mixed models (baked parts plus ops) keep
    /// their parts on disk.
    pub fn parts_derive_from_ops(&self) -> bool {
        self.procedural() && self.derived_from_ops
    }

    /// Marks the cached parts as carrying information beyond the graph (a
    /// baked edit), so saves keep them instead of re-deriving.
    pub fn mark_baked(&mut self) {
        self.derived_from_ops = false;
    }

    pub fn part_mut(&mut self, name: &str) -> Option<&mut Part> {
        // Find first, then copy-on-write only the matched part (mapping
        // `make_mut` over the iterator would un-share every part before it).
        let slot = self.parts.iter_mut().find(|p| p.name == name)?;
        Some(std::sync::Arc::make_mut(slot))
    }

    pub fn annotation_at(&self, id: u32) -> Option<&Annotation> {
        self.annotations.iter().find(|a| a.id == id)
    }

    /// Mean of a part's world-space vertices. A rough
    /// "middle of the part" for pinning annotations without a clicked spot.
    pub fn part_centroid(&self, name: &str) -> Option<[f32; 3]> {
        let p = self.part(name)?;
        if p.verts.is_empty() {
            return Some(p.translation);
        }
        let n = p.verts.len() as f32;
        let sum: Vec3 = (0..p.verts.len()).map(|i| p.world_vert(i)).sum();
        Some((sum / n).to_array())
    }

    /// World bounds of the named parts (hidden or not), or `None` if none of
    /// the names exist. Used to frame a render on part of the model.
    pub fn bounds_of(&self, names: &[String]) -> Option<(Vec3, Vec3)> {
        let mut acc: Option<(Vec3, Vec3)> = None;
        for p in &self.parts {
            if p.verts.is_empty() || !names.contains(&p.name) {
                continue;
            }
            let (min, max) = p.bounds();
            acc = Some(match acc {
                None => (min, max),
                Some((a, b)) => (a.min(min), b.max(max)),
            });
        }
        acc
    }

    /// World bounds over the visible parts only (hidden parts are not rendered
    /// or exported, so they should not frame the camera either).
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        let mut acc: Option<(Vec3, Vec3)> = None;
        for p in &self.parts {
            if p.verts.is_empty() || !p.visible {
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
    ///
    /// A snapshot whose parts are the pure derivation of its ops stores no
    /// parts: they are re-derived on navigation (`Model::restore_parts`),
    /// which the eval cache makes cheap. Memory then grows with the ops, not
    /// with edits x mesh size.
    pub fn record(&mut self, mut model: Model, label: impl Into<String>) -> usize {
        if model.parts_derive_from_ops() {
            model.parts.clear();
        }
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
    pub fn jump(&mut self, id: usize) -> Result<(usize, Model), ModelerError> {
        let Some(node) = self.nodes.get(id) else {
            return Err(format!("no history node #{id} (0..{})", self.nodes.len()).into());
        };
        self.current = Some(id);
        Ok((id, node.model.clone()))
    }

    /// Drops old history: keeps the newest `keep` nodes, plus the cursor, the
    /// root and every branch point (a node with 2+ children), and relinks each
    /// survivor to its nearest surviving ancestor. Node ids are renumbered
    /// (oldest first). Returns how many nodes were removed.
    pub fn prune(&mut self, keep: usize) -> usize {
        let n = self.nodes.len();
        if n <= keep {
            return 0;
        }
        let mut kept = vec![false; n];
        for flag in kept.iter_mut().skip(n - keep) {
            *flag = true;
        }
        for (i, node) in self.nodes.iter().enumerate() {
            if node.children.len() > 1 || node.parent.is_none() {
                kept[i] = true;
            }
        }
        if let Some(c) = self.current {
            kept[c] = true;
        }
        // New index for each survivor.
        let mut remap = vec![usize::MAX; n];
        let mut next = 0;
        for i in 0..n {
            if kept[i] {
                remap[i] = next;
                next += 1;
            }
        }
        let nearest_kept = |mut i: usize, nodes: &[Snapshot]| -> Option<usize> {
            while let Some(p) = nodes[i].parent {
                if kept[p] {
                    return Some(remap[p]);
                }
                i = p;
            }
            None
        };
        let parents: Vec<Option<usize>> = (0..n)
            .map(|i| {
                if kept[i] {
                    nearest_kept(i, &self.nodes)
                } else {
                    None
                }
            })
            .collect();
        let old = std::mem::take(&mut self.nodes);
        let mut nodes: Vec<Snapshot> = Vec::with_capacity(next);
        for (i, mut node) in old.into_iter().enumerate() {
            if kept[i] {
                node.parent = parents[i];
                node.children.clear();
                nodes.push(node);
            }
        }
        for i in 0..nodes.len() {
            if let Some(p) = nodes[i].parent {
                nodes[p].children.push(i);
            }
        }
        self.current = self.current.map(|c| remap[c]);
        self.nodes = nodes;
        n - next
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

/// Defines a closed string-valued enum with `as_str`, `Display` and `FromStr`
/// (the wire format is the snake_case name; serde aliases keep old spellings).
macro_rules! wire_enum {
    ($(#[$m:meta])* $name:ident { $($(#[$vm:meta])* $var:ident = $s:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($(#[$vm])* $var),+ }
        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$var),+];
            pub fn as_str(self) -> &'static str {
                match self { $($name::$var => $s),+ }
            }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
        impl std::str::FromStr for $name {
            type Err = String;
            fn from_str(s: &str) -> Result<Self, String> {
                match s {
                    $($s => Ok($name::$var),)+
                    other => Err(format!(
                        "unknown {} `{other}` (expected one of: {})",
                        stringify!($name),
                        [$($s),+].join(", ")
                    ).into()),
                }
            }
        }
    };
}

wire_enum!(
    /// The built-in primitive shapes.
    PrimitiveKind {
        Box = "box",
        Sphere = "sphere",
        Cylinder = "cylinder",
        Cone = "cone",
        Capsule = "capsule",
        Torus = "torus",
        Plane = "plane",
        Wedge = "wedge",
        Disc = "disc",
        Washer = "washer",
    }
);

impl PrimitiveKind {
    /// Open single sheets, built double-sided.
    pub fn is_open_sheet(self) -> bool {
        matches!(self, Self::Plane | Self::Disc | Self::Washer)
    }
}

wire_enum!(
    /// CSG operations.
    BoolOp {
        Union = "union",
        #[serde(alias = "subtract")]
        Difference = "difference",
        #[serde(alias = "intersection")]
        Intersect = "intersect",
    }
);

wire_enum!(
    /// A coordinate axis.
    Axis {
        X = "x",
        Y = "y",
        Z = "z",
    }
);

impl Axis {
    pub fn index(self) -> usize {
        self as usize
    }
    pub fn unit(self) -> Vec3 {
        match self {
            Axis::X => Vec3::X,
            Axis::Y => Vec3::Y,
            Axis::Z => Vec3::Z,
        }
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
        kind: PrimitiveKind,
        size: [f32; 3],
        translation: [f32; 3],
        /// Optional rotation, quaternion [x, y, z, w]. Saves a
        /// primitive + transform pair for rotated primitives.
        #[serde(default)]
        rotation: Option<[f32; 4]>,
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
        op: BoolOp,
        a: String,
        b: String,
    },
    /// Copies an earlier part around a circle: `count` copies spread over
    /// `degrees` starting at `start_degrees`, rotating about `axis` (default
    /// "y"; "x" and "z" make horizontal rings like windmill blades) through
    /// `center`. With start = step and degrees = 360 - step, the source part
    /// fills the 0° slot evenly.
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
        #[serde(default = "default_axis")]
        axis: Axis,
        /// When true the source part is copy 0 and `count` items are spread
        /// evenly over `degrees` (copies are `name_1..name_{count-1}`), so no
        /// `count-1` / `start_degrees` bookkeeping is needed.
        #[serde(default)]
        include_source: bool,
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
        axis: Axis,
        #[serde(default)]
        offset: [f32; 3],
    },
    /// Shows or hides `input` as output `name` (use the same name for an
    /// in-place edit). Hidden parts keep deriving but are skipped by the
    /// preview and glTF export. Survives graph re-evaluation, unlike a baked
    /// hide.
    Visibility {
        name: String,
        input: String,
        visible: bool,
    },
    /// Renames the part `input` to `name`: the old name stops existing, and
    /// every op that reads the old name still sees it (the rename runs after
    /// them).
    Rename { name: String, input: String },
}

fn default_axis() -> Axis {
    Axis::Y
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centroid_honors_rotation_and_scale() {
        let mut p = Part::new("p");
        p.verts = vec![[1.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 0.0, 0.0]];
        p.scale = [2.0, 1.0, 1.0];
        // 90 degrees about Z: +X maps to +Y.
        let h = std::f32::consts::FRAC_1_SQRT_2;
        p.rotation = [0.0, 0.0, h, h];
        p.translation = [0.0, 0.0, 5.0];
        let mut m = Model::named("m");
        m.parts.push(p.into());
        let c = m.part_centroid("p").unwrap_or_default();
        assert!(
            (c[0]).abs() < 1e-5 && (c[1] - 2.0).abs() < 1e-5 && (c[2] - 5.0).abs() < 1e-5,
            "{c:?}"
        );
    }
}
