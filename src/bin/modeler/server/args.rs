use crate::prelude::*;

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct AddPrimitiveArgs {
    /// Kind of primitive, e.g. "box", "sphere", "cylinder" (see the primitives module).
    pub(crate) kind: PrimitiveKind,
    /// Size in metres: [width, height, depth].
    pub(crate) size: [f32; 3],
    /// Where to put it: [x, y, z].
    pub(crate) translation: [f32; 3],
    /// Unique part name to give it.
    pub(crate) name: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct AddTriMeshArgs {
    /// Unique part name.
    pub(crate) name: String,
    /// Vertex positions [x, y, z], in metres.
    pub(crate) verts: Vec<[f32; 3]>,
    /// Triangle indices into `verts`, exactly three per triangle.
    pub(crate) indices: Vec<u32>,
    /// Optional per-vertex normals; computed automatically when omitted.
    pub(crate) normals: Option<Vec<[f32; 3]>>,
    /// Optional UVs [u, v]; box projection when omitted.
    pub(crate) uvs: Option<Vec<[f32; 2]>>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct BooleanArgs {
    /// Boolean operation: "union", "difference" or "intersection".
    pub(crate) op: BoolOp,
    /// Name of the first part (the base).
    pub(crate) a: String,
    /// Name of the second part (the tool).
    pub(crate) b: String,
    /// Optional name for the result part.
    pub(crate) name: Option<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct TransformPartArgs {
    /// The part to move.
    pub(crate) part: String,
    /// New translation [x, y, z]; unchanged when omitted.
    pub(crate) translation: Option<[f32; 3]>,
    /// New rotation as quaternion [x, y, z, w]; unchanged when omitted.
    pub(crate) rotation: Option<[f32; 4]>,
    /// New scale [x, y, z]; unchanged when omitted.
    pub(crate) scale: Option<[f32; 3]>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct PartArgs {
    /// The part name.
    pub(crate) part: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct RenamePartArgs {
    /// The part to rename.
    pub(crate) part: String,
    /// The new name.
    pub(crate) to: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct SetMaterialArgs {
    /// The part to paint.
    pub(crate) part: String,
    /// Base colour [r, g, b, a], 0..1; unchanged when omitted.
    pub(crate) base_color: Option<[f32; 4]>,
    /// Roughness 0..1 (0 shiny, 1 matte); unchanged when omitted.
    pub(crate) roughness: Option<f32>,
    /// Metalness 0..1; unchanged when omitted.
    pub(crate) metalness: Option<f32>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct SetUvsArgs {
    /// The part to re-project.
    pub(crate) part: String,
    /// UV projection to apply, e.g. "box", "planar", "spherical".
    pub(crate) projection: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct ExportModelArgs {
    /// File path to write, e.g. "chair.glb".
    pub(crate) path: String,
    /// "gltf" for a JSON .gltf or "glb" for a binary .glb.
    pub(crate) format: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct SavePrefabArgs {
    /// Human description of what the model is.
    pub(crate) description: String,
    /// Short search tags, e.g. ["furniture", "wood"].
    pub(crate) tags: Vec<String>,
    /// The prompt/idea that produced this model, for future agents.
    pub(crate) prompt: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct NameArgs {
    /// The new name.
    pub(crate) name: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct AddAnnotationArgs {
    /// The part the comment is about.
    pub(crate) part: String,
    /// The comment text.
    pub(crate) text: String,
    /// Optional triangle index (from add_tri_mesh/boolean output) to pin the comment to.
    pub(crate) face: Option<u32>,
    /// Optional vertex index to pin the comment to.
    pub(crate) vertex: Option<u32>,
    /// Optional world-space point [x, y, z] the comment sits at; defaults to the part's
    /// centroid, or the spot the human clicked in the live view when face/vertex/position
    /// are all omitted.
    pub(crate) position: Option<[f32; 3]>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct AnnotationIdArgs {
    /// The annotation id, as shown by list_annotations.
    pub(crate) id: u32,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct HistoryIdArgs {
    /// The history node id, as shown by history.
    pub(crate) id: usize,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct OpPrimitiveArgs {
    /// Unique output part name; re-using a name replaces that op (parameter tweak).
    pub(crate) name: String,
    /// Primitive kind: box, sphere, cylinder, cone, torus, capsule, plane.
    pub(crate) kind: PrimitiveKind,
    /// Full extents for box/plane; radius for sphere; [radius, height, _] for
    /// cylinder/cone/capsule; [ring, tube, _] for torus.
    pub(crate) size: [f32; 3],
    pub(crate) translation: [f32; 3],
    /// Optional rotation, quaternion [x, y, z, w].
    #[serde(default)]
    pub(crate) rotation: Option<[f32; 4]>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct OpTransformArgs {
    /// Unique output part name; re-using a name replaces that op.
    pub(crate) name: String,
    /// An earlier op's output part.
    pub(crate) input: String,
    #[serde(default)]
    pub(crate) translation: Option<[f32; 3]>,
    /// Quaternion [x, y, z, w].
    #[serde(default)]
    pub(crate) rotation: Option<[f32; 4]>,
    #[serde(default)]
    pub(crate) scale: Option<[f32; 3]>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct OpBooleanArgs {
    /// Unique output part name; re-using a name replaces that op.
    pub(crate) name: String,
    /// union, difference or intersection.
    pub(crate) op: BoolOp,
    pub(crate) a: String,
    pub(crate) b: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct OpRadialArrayArgs {
    /// Output name prefix: copies become name_0 .. name_{count-1}.
    pub(crate) name: String,
    /// An earlier op's output part to copy.
    pub(crate) input: String,
    pub(crate) count: u32,
    /// The circle's center; copies rotate about +Y through this point.
    #[serde(default)]
    pub(crate) center: [f32; 3],
    /// Arc to spread the copies over, in degrees. Default 360 (full ring).
    #[serde(default = "default_tool_degrees")]
    pub(crate) degrees: f32,
    /// Angle of the first copy. Use step (= degrees / count) plus count-1
    /// copies when the source part itself should fill the 0° slot evenly.
    #[serde(default)]
    pub(crate) start_degrees: f32,
    /// Rotation axis: "y" (default), "x" or "z". "x"/"z" make horizontal
    /// rings (windmill blades, fan spokes).
    #[serde(default = "default_axis")]
    pub(crate) axis: Axis,
    /// Simplest way to space items evenly: the source is copy 0 and `count`
    /// items spread over `degrees`; copies are name_1..name_{count-1}. No
    /// count-1 / start_degrees arithmetic needed.
    #[serde(default)]
    pub(crate) include_source: bool,
}

pub(crate) fn default_axis() -> Axis {
    Axis::Y
}

pub(crate) fn default_tool_degrees() -> f32 {
    360.0
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct OpMaterialArgs {
    /// Unique output part name; re-using a name replaces that op.
    pub(crate) name: String,
    /// An earlier op's output part.
    pub(crate) input: String,
    #[serde(default)]
    pub(crate) base_color: Option<[f32; 4]>,
    #[serde(default)]
    pub(crate) roughness: Option<f32>,
    #[serde(default)]
    pub(crate) metalness: Option<f32>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct OpLinearArrayArgs {
    /// Output name prefix: copies become name_1 .. name_{count-1}.
    pub(crate) name: String,
    /// An earlier op's output part to copy.
    pub(crate) input: String,
    pub(crate) count: u32,
    /// Per-copy step vector; copy i sits at input + i * offset.
    #[serde(default)]
    pub(crate) offset: [f32; 3],
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct OpMirrorArgs {
    /// Unique output part name; re-using a name replaces that op.
    pub(crate) name: String,
    /// An earlier op's output part to mirror.
    pub(crate) input: String,
    /// The plane's axis: "x", "y" or "z".
    pub(crate) axis: Axis,
    /// Where the mirror plane sits on that axis, e.g. [0.5, 0, 0] mirrors
    /// across x = 0.5.
    #[serde(default)]
    pub(crate) offset: [f32; 3],
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct OpNameArgs {
    /// The op's output part name, as shown by scene_summary's ops list.
    pub(crate) name: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct CatalogNameArgs {
    /// The prefab/catalog entry name.
    pub(crate) name: String,
}

/// One op in an `apply_ops` batch, tagged by "type" (e.g.
/// {"type": "primitive", "name": "leg", "kind": "cylinder", ...}). Same
/// parameters as the individual op tools.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum OpSpec {
    Primitive {
        /// Unique output part name; re-using a name replaces that op.
        name: String,
        /// Primitive kind: box, sphere, cylinder, cone, capsule, torus, plane, wedge, disc, washer.
        kind: PrimitiveKind,
        /// Full extents for box/plane/wedge; radius for sphere; [radius, height, _] for cylinder/cone/capsule.
        size: [f32; 3],
        translation: [f32; 3],
        /// Optional rotation, quaternion [x, y, z, w].
        #[serde(default)]
        rotation: Option<[f32; 4]>,
    },
    Transform {
        name: String,
        input: String,
        #[serde(default)]
        translation: Option<[f32; 3]>,
        /// Quaternion [x, y, z, w].
        #[serde(default)]
        rotation: Option<[f32; 4]>,
        #[serde(default)]
        scale: Option<[f32; 3]>,
    },
    Boolean {
        name: String,
        /// union, difference or intersection.
        op: BoolOp,
        a: String,
        b: String,
    },
    RadialArray {
        name: String,
        input: String,
        count: u32,
        #[serde(default)]
        center: [f32; 3],
        #[serde(default = "default_tool_degrees")]
        degrees: f32,
        #[serde(default)]
        start_degrees: f32,
        #[serde(default = "default_axis")]
        axis: Axis,
        #[serde(default)]
        include_source: bool,
    },
    LinearArray {
        name: String,
        input: String,
        count: u32,
        #[serde(default)]
        offset: [f32; 3],
    },
    Mirror {
        name: String,
        input: String,
        /// "x", "y" or "z".
        axis: Axis,
        #[serde(default)]
        offset: [f32; 3],
    },
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
    /// Baked edit (not an op): hide a part. Runs in batch position.
    HidePart { part: String },
    /// Baked edit (not an op): un-hide a part. Runs in batch position.
    ShowPart { part: String },
    /// Baked edit (not an op): rename a part. Runs in batch position.
    Rename {
        part: String,
        /// The new name.
        to: String,
    },
}

impl From<OpSpec> for BatchItem {
    fn from(spec: OpSpec) -> Self {
        match spec {
            OpSpec::Primitive {
                name,
                kind,
                size,
                translation,
                rotation,
            } => BatchItem::Op(Op::Primitive {
                name,
                kind,
                size,
                translation,
                rotation,
            }),
            OpSpec::Transform {
                name,
                input,
                translation,
                rotation,
                scale,
            } => BatchItem::Op(Op::Transform {
                name,
                input,
                translation,
                rotation,
                scale,
            }),
            OpSpec::Boolean { name, op, a, b } => BatchItem::Op(Op::Boolean { name, op, a, b }),
            OpSpec::RadialArray {
                name,
                input,
                count,
                center,
                degrees,
                start_degrees,
                axis,
                include_source,
            } => BatchItem::Op(Op::RadialArray {
                name,
                input,
                count,
                center,
                degrees,
                start_degrees,
                axis,
                include_source,
            }),
            OpSpec::LinearArray {
                name,
                input,
                count,
                offset,
            } => BatchItem::Op(Op::LinearArray {
                name,
                input,
                count,
                offset,
            }),
            OpSpec::Mirror {
                name,
                input,
                axis,
                offset,
            } => BatchItem::Op(Op::Mirror {
                name,
                input,
                axis,
                offset,
            }),
            OpSpec::Material {
                name,
                input,
                base_color,
                roughness,
                metalness,
            } => BatchItem::Op(Op::Material {
                name,
                input,
                base_color,
                roughness,
                metalness,
            }),
            OpSpec::HidePart { part } => BatchItem::Hide { part },
            OpSpec::ShowPart { part } => BatchItem::Show { part },
            OpSpec::Rename { part, to } => BatchItem::Rename { part, to },
        }
    }
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct ApplyOpsArgs {
    /// Ops to apply in order, each tagged with its "type".
    pub(crate) ops: Vec<OpSpec>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct SetCameraArgs {
    /// Horizontal angle in degrees (0 looks down -z).
    pub(crate) yaw: f32,
    /// Vertical angle in degrees, -89..89.
    pub(crate) pitch: f32,
    /// Distance from the orbit target.
    pub(crate) dist: f32,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct ExpectPartsArgs {
    /// The exact part names the model should have.
    pub(crate) parts: Vec<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct ExpectBoundsArgs {
    /// Expected world-bounds minimum per axis; `null` skips that axis.
    pub(crate) min: [Option<f32>; 3],
    /// Expected world-bounds maximum per axis; `null` skips that axis.
    pub(crate) max: [Option<f32>; 3],
    /// Allowed deviation per checked axis. Default 0.01.
    #[serde(default = "default_tol")]
    pub(crate) tol: f32,
}

pub(crate) fn default_tol() -> f32 {
    0.01
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct AgentShotArgs {
    /// Which view: "front", "back", "left", "right", "top", "bottom"
    /// (orthographic, auto-framed) or "iso" (perspective three-quarter).
    pub(crate) view: String,
    /// Pixel width. Default 1280.
    #[serde(default = "default_shot_width")]
    pub(crate) width: u32,
    /// Pixel height. Default 720.
    #[serde(default = "default_shot_height")]
    pub(crate) height: u32,
    /// Frame only these parts (hidden ones included) instead of the whole
    /// model, e.g. ["tower_final", "roof"] to zoom in on the building.
    #[serde(default)]
    pub(crate) focus: Option<Vec<String>>,
}

pub(crate) fn default_shot_width() -> u32 {
    1280
}

pub(crate) fn default_shot_height() -> u32 {
    720
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct AgentViewsArgs {
    /// Views to render, e.g. ["front", "right", "top"] for a three-pane check
    /// or ["iso"] for the looks-right shot. Each returns its own image.
    pub(crate) views: Vec<String>,
    /// Pixel width per view. Default 1280.
    #[serde(default = "default_shot_width")]
    pub(crate) width: u32,
    /// Pixel height per view. Default 720.
    #[serde(default = "default_shot_height")]
    pub(crate) height: u32,
    /// Frame only these parts (hidden ones included) instead of the whole model.
    #[serde(default)]
    pub(crate) focus: Option<Vec<String>>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct SessionLogArgs {
    /// How many recent calls to show. Default 30, max 500.
    pub(crate) limit: Option<u32>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct ProbeRayArgs {
    /// Ray origin, world space [x, y, z].
    pub(crate) origin: [f32; 3],
    /// Ray direction (normalized internally).
    pub(crate) dir: [f32; 3],
    /// List every part the ray passes through, nearest first, instead of only
    /// the first hit (to see what is behind an opening). Default false.
    #[serde(default)]
    pub(crate) all_hits: bool,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct HistoryPruneArgs {
    /// How many of the newest snapshots to keep (the cursor, root and branch
    /// points are always kept as well).
    pub(crate) keep: usize,
}
